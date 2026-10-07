//! The queue-helpers family (lib.sh decomposition row AA; sp-hwjsq, "wave 4.32"), ported
//! into queue, their owning crate. Four of the seven lib.sh functions this family named —
//! `queue_notify_concierge`, `queue_local_check_divergence`, `queue_cancel_branch_runs`,
//! `queue_is_suite_transition` — had no caller left outside this crate's own lib.sh seam
//! (`queue/src/seam.rs`'s now-removed `Op::Notify`/`Divergence`/`CancelRuns`, and
//! `queue_sort_rows`'s internal call to the transition check); once that seam goes
//! in-process, below, they are retired outright rather than kept as shims nobody calls.
//!
//! The other three keep external callers and so keep a one-line lib.sh shim onto `queue`'s
//! own CLI (`main.rs`): `queue_certified_list` (`queue-certified-list.sh`, cockpit-collect),
//! `queue_sort_rows` (`test-queue-sort-large.sh`, cockpit-collect) and `spira_git_push`
//! (`branch-sweep.sh`, `test-git-push-app.sh`, and landing-pass's own separate seam, a
//! different crate this bead does not touch).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::ports::{Divergence, Git, Lc, World};
use crate::real::{RealGit, RealLc};

/// `queue_certified_list <repo-path>`: every `spira/*` branch whose lifecycle row is
/// CERTIFIED, as `(id, tip, epoch)` — the selection primitive any cutter (the batcher, the
/// reconciler's mergeability check, cockpit-collect's "next up" pane) draws from.
pub fn certified_list(repo: &Path) -> Vec<(String, String, u64)> {
    let lc = RealLc { bin: Some(PathBuf::from("spira-lc")) };
    certified_rows(&lc, &RealGit.branches(repo, "refs/heads/spira/"))
}

/// The CERTIFIED rows whose bead has a `spira/<id>` branch among `branches`.
pub fn certified_rows(lc: &dyn Lc, branches: &[(String, String)]) -> Vec<(String, String, u64)> {
    let rows = lc.bead_rows(Some("CERTIFIED")).unwrap_or_default();
    let mut out = Vec::new();
    for (branch, _) in branches {
        let Some(id) = branch.strip_prefix("spira/") else { continue };
        if let Some(r) = rows.iter().find(|r| r.bead_id == id) {
            out.push((id.to_string(), r.tip.clone().unwrap_or_default(), r.since.unwrap_or(0)));
        }
    }
    out
}

/// `queue_is_suite_transition <repo> <tip> <base>`: true when `tip` touches
/// `$SPIRA_SUITE_STATE_FILE` (default `spira/suite-state`) relative to `base` — a
/// suite-state edit is a tiebreaker within one priority, never a rank of its own
/// (sp-ihxa0).
pub fn is_suite_transition(repo: &Path, tip: &str, base: &str) -> bool {
    let file = match spira_config::resolve::suite_state_file() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("queue: {e}");
            return false;
        }
    };
    let out = Command::new("git").arg("-C").arg(repo).args(["diff", "--name-only", base, tip]).stdin(Stdio::null()).stderr(Stdio::null()).output();
    match out {
        // grep -F: a fixed-string SUBSTRING match against any changed-path line, not an
        // exact-line match — preserved here rather than tightened, to keep the port's
        // output contract byte for byte.
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).lines().any(|l| l.contains(file.as_str())),
        _ => false,
    }
}

/// Parse "<id> <tip> <epoch> …" lines the way the bash `read -r _id _tip _epoch` loop
/// (followed by `${_epoch%% *}`) did: only the first three whitespace-separated tokens
/// matter, and a line short of three is dropped silently (never a row in the output,
/// ranked or unranked — true of the original on both paths).
pub fn parse_rows(text: &str) -> Vec<(String, String, i64)> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let id = it.next()?.to_string();
            let tip = it.next()?.to_string();
            let epoch: i64 = it.next()?.parse().ok()?;
            Some((id, tip, epoch))
        })
        .collect()
}

/// One ranked `queue_sort_rows` row in its own text columns. `express`/`trans` are 0 when
/// true, 1 when false, so an ascending sort puts "true" first — the same encoding the
/// bash function's own sort key used.
pub struct RankedRow {
    pub express: i64,
    pub prio: i64,
    pub trans: i64,
    pub epoch: i64,
    pub id: String,
    pub tip: String,
}

/// `<express> <prio padded to 9> <trans> <epoch padded to 10> <id> <tip>` — the exact
/// columns `queue_sort_rows` printed (callers outside this crate read `$5`/`$2` off it).
pub fn render_row(r: &RankedRow) -> String {
    format!("{} {:09} {} {:010} {} {}", r.express, r.prio, r.trans, r.epoch, r.id, r.tip)
}

fn value_as_i64(v: &serde_json::Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// `queue_sort_rows`'s ranking (lib.sh's own comment, carried forward verbatim in spirit):
/// express first, then priority ascending; a `spira/suite-state` transition is only a
/// tiebreaker WITHIN one priority, never ahead of it (sp-ihxa0); certification age
/// (epoch, lowest first) breaks whatever tie remains.
///
/// FAILS OPEN. Ranking is an optimisation; emptying the queue because it broke is the
/// catastrophic outcome (sp-m5iq3: a 266 KiB `PRIO_JSON` once blew past Linux's 128 KiB
/// single-env-string limit, every exec inside the old bash failed E2BIG, and the batcher
/// cut nothing for nine hours with 40 branches certified and waiting). So unparseable
/// `prio_json` returns every row in its ORIGINAL order, never dropped, with a warning
/// naming why ranking was skipped.
pub fn sort_rows(rows: &[(String, String, i64, bool)], prio_json: &str, express_label: &str) -> (Vec<RankedRow>, Option<String>) {
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(prio_json);
    let Ok(parsed) = parsed else {
        let unranked = rows
            .iter()
            .map(|(id, tip, epoch, is_trans)| RankedRow { express: 1, prio: 9, trans: if *is_trans { 0 } else { 1 }, epoch: *epoch, id: id.clone(), tip: tip.clone() })
            .collect();
        return (unranked, Some("queue_sort_rows: ranking failed (rc=1) -- returning rows unranked".to_string()));
    };
    let items: Vec<&serde_json::Value> = match &parsed {
        serde_json::Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let mut prio_map: HashMap<String, i64> = HashMap::new();
    let mut express_set: HashSet<String> = HashSet::new();
    for x in items {
        if !x.is_object() {
            continue;
        }
        let Some(id) = x.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else { continue };
        let prio = x.get("priority").and_then(value_as_i64).unwrap_or(9);
        prio_map.insert(id.to_string(), prio);
        let has_label = x.get("labels").and_then(|v| v.as_array()).is_some_and(|a| a.iter().any(|l| l.as_str() == Some(express_label)));
        if has_label {
            express_set.insert(id.to_string());
        }
    }
    let mut keyed: Vec<(i64, i64, i64, i64, String, String)> = rows
        .iter()
        .map(|(id, tip, epoch, is_trans)| {
            let express = if express_set.contains(id) { 0 } else { 1 };
            let prio = *prio_map.get(id).unwrap_or(&9);
            let trans = if *is_trans { 0 } else { 1 };
            (express, prio, trans, *epoch, id.clone(), tip.clone())
        })
        .collect();
    keyed.sort();
    let ranked = keyed.into_iter().map(|(express, prio, trans, epoch, id, tip)| RankedRow { express, prio, trans, epoch, id, tip }).collect();
    (ranked, None)
}

fn append_log(run_dir: &Path, name: &str, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(run_dir.join(name)) {
        let _ = writeln!(f, "{line}");
    }
}

/// `queue_cancel_branch_runs <forge-bin> <repo> <branch> [tag]`: cancel every non-completed
/// Gate run on `branch` and log each attempt to `landing.log`. GitHub does not cancel a
/// workflow run when its PR closes, and each batch branch is a fresh `spira/queue/<stamp>`,
/// so the `gate-${ref}` concurrency group has no earlier run on that branch to collide with
/// and cancel for free — closing the PR must cancel the run itself. A failed cancel is
/// logged loudly (stderr), never swallowed: the run stays non-completed and its PR stays
/// closed, so the next abandon retries it.
pub fn cancel_branch_runs(forge: &Path, repo: &Path, branch: &str, tag: &str) -> bool {
    if branch.is_empty() {
        return true;
    }
    let run_dir = match spira_config::resolve::run_dir_for_process() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("queue: {e}");
            return false;
        }
    };
    let listing = Command::new(forge)
        .arg("runs-for-branch")
        .arg(repo)
        .arg(branch)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let mut all_ok = true;
    for line in listing.lines() {
        let mut it = line.split_whitespace();
        let Some(run_id) = it.next() else { continue };
        if run_id.is_empty() {
            continue;
        }
        let status = it.next().unwrap_or("");
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let cancelled = Command::new(forge).arg("run-cancel").arg(repo).arg(run_id).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        if cancelled {
            append_log(&run_dir, "landing.log", &format!("{tag} RUN_CANCEL {now} branch={branch} run={run_id} status={status}"));
        } else {
            all_ok = false;
            append_log(&run_dir, "landing.log", &format!("{tag} RUN_CANCEL_FAILED {now} branch={branch} run={run_id} status={status}"));
            eprintln!("spira: WARN failed to cancel run {run_id} for {branch} — will retry");
        }
    }
    all_ok
}

/// `queue_notify_concierge <name> <subject-suffix> <body>`: mails `mailbox`
/// (`Settings::mailbox`, i.e. `SPIRA_MAIL_SESSION_MAILBOX`'s declared value) as a machine
/// event for a mutation the owner just made to an open batch (eject, rebuild, force-push,
/// merge). spira-mail-deliver.sh watches every registered mailbox and wakes its reader the
/// moment new mail lands (law-machine-events-wake-in-real-time), so the Concierge learns of
/// it within seconds — never by polling the queue by hand. Best-effort, as the bash body
/// was (`|| true`): a mail failure never blocks the mutation it is reporting on.
pub fn notify(mailbox: &str, name: &str, subject: &str, body: &str) {
    let full_subject = format!("Merge queue: {name} {subject}");
    let full_body = format!("## Alert\n{body}\n");
    if let Ok(mut child) = Command::new("mail")
        .args(["send", mailbox, "--from", "Spira Queue <queue@spira>", "--subject", &full_subject, "--kind", "alert"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        use std::io::Write;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(full_body.as_bytes());
        }
        let _ = child.wait();
    }
}

/// Row 4 of the local/main design: is `forge_sha` an ancestor of `local_sha`? Under
/// queue.local the forge's main moves only by our own publishes, so a non-ancestor means
/// something pushed outside the publish queue; the first sighting of each foreign tip mails
/// `mailbox` (marker `queue/<name>/divergence-alarmed`, cleared once healthy). Never
/// rebases. A check that cannot run is `CannotCheck`, never `Diverged`.
pub fn check_divergence(mailbox: &str, queue_dir: &Path, name: &str, repo: &Path, forge_sha: &str, local_sha: &str) -> Divergence {
    if queue_dir.as_os_str().is_empty() {
        return Divergence::CannotCheck("the queue directory is not configured, so the divergence marker cannot be placed".into());
    }
    let statefile = queue_dir.join(name).join("divergence-alarmed");
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", forge_sha, local_sha])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => {
            let _ = std::fs::remove_file(&statefile);
            return Divergence::Ancestor;
        }
        Ok(s) if s.code() == Some(1) => {}
        Ok(s) => return Divergence::CannotCheck(format!("git merge-base --is-ancestor {forge_sha} {local_sha} failed ({s}) in {}", repo.display())),
        Err(e) => return Divergence::CannotCheck(format!("could not run git in {}: {e}", repo.display())),
    }
    let foreign_range = format!("{local_sha}..{forge_sha}");
    let already = std::fs::read_to_string(&statefile).ok().map(|s| s.trim().to_string()).unwrap_or_default();
    if already != forge_sha {
        let foreign = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["log", "--format=%h %s", &foreign_range])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
            .unwrap_or_default();
        if let Some(parent) = statefile.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&statefile, format!("{forge_sha}\n"));
        let body = format!(
            "{name}'s forge target ({forge_sha}) is not an ancestor of local/main ({local_sha}) — something pushed to the forge outside the publish queue. Foreign commit(s):\n{}\n\nPublishing is refused until this is reconciled by hand. Never rebase silently.",
            if foreign.is_empty() { "<none found>" } else { &foreign }
        );
        notify(mailbox, name, "divergence: forge is not an ancestor of local/main", &body);
    }
    Divergence::Diverged(foreign_range)
}

/// `spira_git_push <repo> [push-args...]`: push with the GitHub App identity when
/// `SPIRA_GH_APP_ID`/`SPIRA_GH_APP_INSTALLATION_ID` are set, routing the push over HTTPS
/// using the App installation token as the credential, so pushes are attributed to the
/// App rather than to the operator's SSH key. Returns the built (not yet run) `Command` so
/// callers can choose stdio: queue's own in-process push (R12) discards stderr as the old
/// seam body did; the CLI shim for bash callers (`branch-sweep.sh`, landing-pass's own
/// separate seam) inherits it, since `spira_git_push` itself never redirected anything —
/// that was always the call site's choice.
pub fn git_push_cmd(repo: &Path, args: &[String]) -> Command {
    let app_configured = std::env::var("SPIRA_GH_APP_ID").ok().filter(|s| !s.is_empty()).is_some() && std::env::var("SPIRA_GH_APP_INSTALLATION_ID").ok().filter(|s| !s.is_empty()).is_some();
    let mut c = Command::new("git");
    c.arg("-C").arg(repo);
    if app_configured {
        c.args(["-c", "credential.helper=!git-credential-app.sh", "-c", "url.https://github.com/.insteadOf=git@github.com:"]);
    }
    c.arg("push").args(args);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, tip: &str, epoch: i64, is_trans: bool) -> (String, String, i64, bool) {
        (id.into(), tip.into(), epoch, is_trans)
    }

    #[test]
    fn parse_rows_drops_short_lines_and_ignores_extra_fields() {
        let rows = parse_rows("sp-a ta 6\nsp-b tb 5 extra junk\nonly-two fields\n\n");
        assert_eq!(rows, vec![("sp-a".to_string(), "ta".to_string(), 6), ("sp-b".to_string(), "tb".to_string(), 5)]);
    }

    #[test]
    fn unparseable_prio_json_fails_open_in_original_order_with_a_warning() {
        let rows = [row("sp-a", "ta", 6, false), row("sp-b", "tb", 5, true)];
        let (ranked, warn) = sort_rows(&rows, "{not json", "express");
        assert!(warn.unwrap().contains("ranking failed"));
        let ids: Vec<&str> = ranked.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["sp-a", "sp-b"], "original order preserved, nothing dropped");
        assert_eq!(ranked[0].prio, 9);
        assert_eq!(ranked[0].express, 1);
        assert_eq!(ranked[1].trans, 0, "is_trans=true renders as the 0 (first) slot even unranked");
    }

    #[test]
    fn priority_orders_rows_and_wins_over_a_transition_tiebreak() {
        let rows = [row("sp-p1", "t1", 2, true), row("sp-p0", "t2", 1, false)];
        let (ranked, warn) = sort_rows(&rows, r#"[{"id":"sp-p0","priority":0},{"id":"sp-p1","priority":1}]"#, "express");
        assert!(warn.is_none());
        let ids: Vec<&str> = ranked.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["sp-p0", "sp-p1"], "a P0 without a transition still sorts ahead of a P1 with one");
    }

    #[test]
    fn within_one_priority_a_transition_sorts_first_despite_a_later_epoch() {
        let rows = [row("sp-p", "tp", 1, false), row("sp-t", "tt", 2, true)];
        let (ranked, _) = sort_rows(&rows, r#"[{"id":"sp-t","priority":2},{"id":"sp-p","priority":2}]"#, "express");
        let ids: Vec<&str> = ranked.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["sp-t", "sp-p"]);
    }

    #[test]
    fn express_ranks_ahead_of_everything_else() {
        let rows = [row("sp-hi-prio", "t1", 1, false), row("sp-express", "t2", 9, false)];
        let (ranked, _) = sort_rows(&rows, r#"[{"id":"sp-hi-prio","priority":0},{"id":"sp-express","priority":9,"labels":["express"]}]"#, "express");
        let ids: Vec<&str> = ranked.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["sp-express", "sp-hi-prio"]);
    }

    #[test]
    fn malformed_prio_entries_default_to_priority_nine_rather_than_crashing() {
        let rows = [row("sp-a", "ta", 1, false)];
        let (ranked, warn) = sort_rows(&rows, r#"[{"id":"sp-a","priority":"not-a-number"}]"#, "express");
        assert!(warn.is_none());
        assert_eq!(ranked[0].prio, 9);
    }

    #[test]
    fn render_row_pads_priority_and_epoch() {
        let r = RankedRow { express: 0, prio: 2, trans: 1, epoch: 42, id: "sp-a".into(), tip: "deadbeef".into() };
        assert_eq!(render_row(&r), "0 000000002 1 0000000042 sp-a deadbeef");
    }
}

/// The close reason a landed member's bead carries (law-closed-is-not-landed).
pub fn land_close_reason(sha: &str) -> String {
    let shown = if sha.is_empty() { "unknown" } else { sha };
    format!("OUTCOME: landed\nClosed by the landing pass: work landed at {shown} (law-closed-is-not-landed).\n")
}

/// Close a landed member's bead and reap its branch, in-process — no landing-pass oracle
/// and no second ledger: the queue records LANDED on spira-lc (`lc_deliver` / the batch's
/// `land` cascade), the one record. A bead never marked submitted is left alone; the close
/// itself is idempotent and refused by spira-lc for a row not in delivery. Best-effort throughout — a failed close is left
/// submitted for CHECK 5, a missed reap is left for the Sending.
pub fn close_on_land(w: &World, submitted_label: &str, id: &str, sha: &str) {
    let Ok(rows) = w.bd.show(&[id.to_string()]) else { return };
    let Some(row) = rows.iter().find(|r| r.id == id) else { return };
    if !row.labels.iter().any(|l| l == submitted_label) {
        return;
    }
    let shown = if sha.is_empty() { "unknown" } else { sha };
    if !w.lib.bead_close(id, &land_close_reason(sha)) {
        w.out(format!("land-close {id}: bd close failed — left submitted (LANDED is on the lifecycle record)"));
        return;
    }
    w.out(format!("land-close {id}: closed at {shown} (submitted -> landed)"));
    let label = |p: &str| row.labels.iter().find_map(|l| l.strip_prefix(p).map(str::to_string));
    let (Some(repo), Some(branch)) = (label("repo:"), label("branch:")) else { return };
    match w.lib.reap_landed_branch(id, &repo, &branch, &format!("landed at {shown}")) {
        Ok(true) => w.out(format!("land-close {id}: reaped branch {branch}")),
        Ok(false) => {}
        Err(e) => w.out(format!("land-close {id}: branch {branch} not reaped: {e} — left for the Sending")),
    }
}
