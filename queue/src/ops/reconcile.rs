//! The forge batch reconciler: one idempotent step per verdict pass that makes the open
//! batch record and its lifecycle row agree with the PR (merged lands, closed or emptied
//! abandons, no PR past the grace abandons).

use std::path::Path;

use super::{idents, landing_log, lc_abandon, Ctx, World, FAIL, OK};
use crate::ident::bounded_text;
use crate::model::Member;
use crate::records::{one_line, write_atomic, Kv};

const ALERT_KEY: &str = "reconcile_alert";

enum Verdict {
    Land,
    Abandon(String),
}

/// `Some(rc)` when this pass was spent converging the batch; `None` when the batch is live
/// and the ordinary verdict pass should read its CI.
pub fn step(w: &World, c: &Ctx, path: &Path, file: &Path, kv: &Kv) -> Option<i32> {
    let pr = kv.get("pr").unwrap_or("").to_string();
    let members = kv.members();
    let verdict = if members.is_empty() && kv.get_first("members").is_some() {
        Verdict::Abandon("the batch has no members".into())
    } else if pr.is_empty() {
        let opened: u64 = kv.get("opened").and_then(|o| o.trim().parse().ok()).unwrap_or(0);
        if opened == 0 || w.clock.now().saturating_sub(opened) < c.s.verdict.ci_maxsec {
            return None;
        }
        Verdict::Abandon(format!("no PR opened within {}s", c.s.verdict.ci_maxsec))
    } else {
        if ["head", "base"].iter().any(|k| kv.get(k).unwrap_or("").is_empty()) {
            return None;
        }
        if idents(w, "verdict", &[("pr", &pr)]).is_err() {
            return Some(FAIL);
        }
        match w.forge.pr_state(&c.s.forge, path, &pr).as_deref().map(str::trim) {
            Some("merged") => Verdict::Land,
            Some("closed") => Verdict::Abandon(format!("PR {pr} was closed without merging")),
            _ => return None,
        }
    };
    Some(match verdict {
        Verdict::Land => land(w, c, path, file, kv, &pr, &members),
        Verdict::Abandon(why) => abandon(w, c, path, file, kv, &pr, &members, &why),
    })
}

/// Alert once per condition: the first failure mails and is recorded on the batch record, a
/// repeat pass logs and stays quiet.
fn cannot_act(w: &World, c: &Ctx, file: &Path, kv: &Kv, what: &str) -> i32 {
    let name = c.r.name.as_str();
    w.err(format!("verdict {name}: reconcile: {what}"));
    if kv.get(ALERT_KEY) == Some(what) {
        return FAIL;
    }
    let mut next = kv.clone();
    next.remove(ALERT_KEY);
    next.push(ALERT_KEY, &one_line(what));
    if write_atomic(file, &next.render()).is_ok() {
        w.lib.notify(&c.s.mailbox, name, "forge batch cannot converge", &format!("{name}: {what}\nThe record is kept; each verdict pass retries and logs, this mail is sent once."));
    }
    FAIL
}

fn terminal(state: &str) -> bool {
    matches!(state, "LANDED" | "SETTLED" | "ABANDONED" | "DISCARDED")
}

fn land(w: &World, c: &Ctx, path: &Path, file: &Path, kv: &Kv, pr: &str, members: &[Member]) -> i32 {
    let name = c.r.name.as_str();
    let head = kv.get("head").unwrap_or("").to_string();
    let base = c.r.landref.clone().unwrap_or_default();
    let sha = w.forge.pr_merge_commit(&c.s.forge, path, pr).filter(|s| idents(w, "verdict", &[("sha", s)]).is_ok()).or_else(|| {
        (!head.is_empty() && !base.is_empty() && w.git.is_ancestor(path, &head, &base)).then(|| head.clone())
    });
    let Some(sha) = sha else {
        return cannot_act(w, c, file, kv, &format!("PR {pr} merged but no merge commit is readable and head {head} is not on {base}"));
    };
    if let Some(id) = kv.get("batch_id").filter(|b| !b.is_empty()) {
        match w.lc.batch_state(id) {
            Some((s, _)) if matches!(s.as_str(), "LANDED" | "SETTLED") => {}
            Some((s, _)) if terminal(&s) => return cannot_act(w, c, file, kv, &format!("PR {pr} merged but batch {id} is already {s} on spira-lc")),
            Some((mut s, v)) => {
                let Ok(mut v) = v.trim().parse::<u64>() else { return cannot_act(w, c, file, kv, &format!("batch {id}: unreadable version {v:?}")) };
                let run = format!("{{\"CiStarted\":{{\"run\":\"{pr}\"}}}}");
                let walk: &[(&str, &str, &str)] = &[("OPEN", &run, "CI_RUNNING"), ("CI_RUNNING", "\"Green\"", "GREEN")];
                for (from, kind, to) in walk {
                    if s == *from {
                        if let Err((rc, e)) = w.lc.batch_event(id, &s, &v.to_string(), "reconcile", kind) {
                            return cannot_act(w, c, file, kv, &format!("spira-lc refused {kind} for batch {id} (rc={rc}): {e}"));
                        }
                        s = (*to).to_string();
                        v += 1;
                    }
                }
                if s != "GREEN" {
                    return cannot_act(w, c, file, kv, &format!("batch {id} is {s} on spira-lc, not a state a merged PR can land from"));
                }
                if let Err((rc, e)) = w.lc.land_batch(id, &v.to_string(), "reconcile", &sha) {
                    return cannot_act(w, c, file, kv, &format!("spira-lc refused land for batch {id} (rc={rc}): {e}"));
                }
                w.out(format!("verdict {name}: reconcile: {id} landed on spira-lc (sha={sha})"));
            }
            None => return cannot_act(w, c, file, kv, &format!("spira-lc has no readable row for batch {id}")),
        }
    }
    for m in members {
        w.lib.gh_issue_closeout(&m.id, &sha, path);
        super::helpers::close_on_land(w, &c.s.submitted_label, &m.id, &sha);
    }
    retire_branch(w, c, path, kv);
    let _ = std::fs::remove_file(file);
    landing_log(&c.s.run, &format!("QUEUE RECONCILE_LANDED {} repo={name} pr={pr} sha={sha}", w.clock.now()));
    w.lib.notify(&c.s.mailbox, name, &format!("PR {pr} merged (reconciled)"), &format!("PR {pr} was merged on the forge; the batch was landed on spira-lc at {sha}."));
    w.out(format!("verdict {name}: reconcile: PR {pr} merged — batch landed at {sha}"));
    OK
}

#[allow(clippy::too_many_arguments)]
fn abandon(w: &World, c: &Ctx, path: &Path, file: &Path, kv: &Kv, pr: &str, members: &[Member], why: &str) -> i32 {
    let name = c.r.name.as_str();
    if let Some(id) = kv.get("batch_id").filter(|b| !b.is_empty()) {
        let r = bounded_text(&format!("reconciled: {why}"));
        match w.lc.batch_state(id) {
            Some((s, _)) if terminal(&s) => {}
            _ => {
                if let Err((rc, e)) = lc_abandon(w, id, "reconcile", &r) {
                    return cannot_act(w, c, file, kv, &format!("spira-lc refused abandon for batch {id} (rc={rc}): {e}"));
                }
            }
        }
    }
    if !pr.is_empty() && w.forge.pr_state(&c.s.forge, path, pr).as_deref().map(str::trim) == Some("open") {
        w.lib.cancel_runs(&c.s.forge, path, kv.get("branch").unwrap_or(""));
        w.forge.pr_close(&c.s.forge, path, pr);
    }
    retire_branch(w, c, path, kv);
    let ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
    let ids = if ids.is_empty() { "<none>".to_string() } else { ids.join(" ") };
    let mut archived = kv.clone();
    archived.remove(ALERT_KEY);
    archived.push("reason", &one_line(why));
    archived.push("actor", "reconcile");
    let archive = c.queue_file(&format!("closed-pr{pr}-{}", w.clock.stamp()));
    if write_atomic(file, &archived.render()).is_err() || std::fs::rename(file, &archive).is_err() {
        return cannot_act(w, c, file, kv, "the open record could not be archived");
    }
    landing_log(&c.s.run, &format!("QUEUE RECONCILE_ABANDONED {} repo={name} pr={pr} members={ids} reason={}", w.clock.now(), one_line(why)));
    w.lib.notify(&c.s.mailbox, name, "forge batch abandoned (reconciled)", &format!("{why}. Batch abandoned; members {ids} are back to CERTIFIED."));
    w.out(format!("verdict {name}: reconcile: batch abandoned — {why}"));
    OK
}

fn retire_branch(w: &World, c: &Ctx, path: &Path, kv: &Kv) {
    let branch = kv.get("branch").unwrap_or("");
    if branch.is_empty() || idents(w, "verdict", &[("branch", branch)]).is_err() {
        return;
    }
    if let Some(remote) = c.r.landref.as_deref().and_then(|b| c.r.ref_remote(b)) {
        w.lib.push(path, &remote, &format!(":refs/heads/{branch}"));
    }
    w.git.branch_delete_sanctioned(path, branch);
}
