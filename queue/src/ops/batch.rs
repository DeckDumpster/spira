//! The queue.forge open batch: eject, abandon, open-batch (DESIGN.md §2.2).

use std::fs;

use super::{
    actor, converge_orphans, czar_ok, idents, landing_log, lc_abandon, lc_eject_member,
    owner_refused, read_text, repo_path, require_lc, resolve, statute_gate, take_lock, title_line,
    World, FAIL, OK, USAGE,
};
use crate::cli::Text;
use crate::ident::bounded_text;
use crate::model::{EjectCause, LandMode, Member};
use crate::ports::ref_branch;
use crate::records::{self, one_line, write_atomic, Kv};

/// A hand eject's walk on spira-lc: Deliver first when the row is still CERTIFIED or SUBMITTED (a requeue of a member delivered from SUBMITTED lands there; Returned is
/// legal only from IN_DELIVERY), then Returned{batch-ejected} -> REWORK; a row already in REWORK is the target state, not a refusal. The refusal is
/// reported here and handed back: an eject that leaves the row where it was must exit non-zero.
#[allow(clippy::result_unit_err)]
pub fn lc_return(w: &World, id: &str, red: bool) -> Result<(), ()> {
    let fail = |why: String| {
        w.err(format!(
            "queue.sh eject: spira-lc: {id}: {why} — not returned to REWORK on spira-lc"
        ));
        Err(())
    };
    let Some((mut state, version)) = w.lc.bead_state(id) else {
        return fail("no lifecycle row".into());
    };
    let Ok(mut v) = version.trim().parse::<u64>() else {
        return fail(format!("unreadable version {version:?}"));
    };
    if state == "REWORK" {
        return Ok(());
    }
    let who = "queue.sh";
    if state == "CERTIFIED" || state == "SUBMITTED" {
        if let Err((rc, e)) =
            w.lc.bead_event(id, &state, &v.to_string(), who, "\"Deliver\"")
        {
            return fail(format!("Deliver refused (rc={rc}): {e}"));
        }
        state = "IN_DELIVERY".into();
        v += 1;
    }
    if state != "IN_DELIVERY" {
        return fail(format!(
            "in state {state}, not SUBMITTED, CERTIFIED or IN_DELIVERY"
        ));
    }
    let reason = if red {
        "batch-ejected-red"
    } else {
        "batch-ejected"
    };
    if let Err((rc, e)) = w.lc.bead_event(
        id,
        &state,
        &v.to_string(),
        who,
        &format!("{{\"Returned\":{{\"reason\":\"{reason}\"}}}}"),
    ) {
        return fail(format!("Returned refused (rc={rc}): {e}"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn eject(
    w: &World,
    id: &str,
    repo: Option<&str>,
    reason: &Text,
    suites: &str,
    red: bool,
    harness_fault: bool,
    dry_run: bool,
) -> i32 {
    if !czar_ok(w) {
        return FAIL;
    }
    let reason = match read_text(w, reason) {
        Ok(r) => r,
        Err(e) => {
            w.err(format!("queue.sh eject: cannot read the reason: {e}"));
            return USAGE;
        }
    };
    let reason = match statute_gate(w, "eject", &reason) {
        Ok(Some(line)) => format!("{reason}\n\n{line}"),
        Ok(None) => reason,
        Err(rc) => return rc,
    };
    let Ok(c) = resolve(w, "eject", repo) else {
        return FAIL;
    };
    let Ok(path) = repo_path(w, "eject", &c) else {
        return FAIL;
    };
    if idents(w, "eject", &[("bead id", id), ("repo", &c.r.name)]).is_err() {
        return FAIL;
    }
    // --suites is a comma-separated list (queue.sh's contract, and what recertification
    // reads back): each name is an identifier, the commas are not.
    if !suites.is_empty()
        && suites
            .split(',')
            .try_for_each(|s| idents(w, "eject", &[("suite", s)]))
            .is_err()
    {
        return FAIL;
    }
    let actor = actor(w);
    let cause = EjectCause::decide(red, harness_fault, suites);
    if !dry_run && require_lc(w, "eject").is_err() {
        return FAIL;
    }
    let Ok(_g) = take_lock(w, "eject", &c, "") else {
        return FAIL;
    };

    let open = match super::forge_batch::read(w, &c) {
        Ok(o) => o,
        Err(e) => {
            w.err(format!("queue.sh eject: cannot tell whether {} has an open batch: {e} — refused, nothing changed", c.r.name));
            return FAIL;
        }
    };
    let kv = open.as_ref().map(|o| o.kv.clone());
    let members = kv.as_ref().map(Kv::members).unwrap_or_default();
    let pr = kv
        .as_ref()
        .and_then(|k| k.get_first("pr"))
        .unwrap_or("")
        .to_string();
    let batch_id = kv
        .as_ref()
        .and_then(|k| k.get_first("batch_id"))
        .unwrap_or("")
        .to_string();
    let hit = members.iter().find(|m| m.id == id).cloned();
    let survivors: Vec<Member> = members.iter().filter(|m| m.id != id).cloned().collect();

    if hit.is_some() {
        let owner = kv.as_ref().and_then(|k| k.get("owner")).unwrap_or("");
        if owner_refused(w, owner, &actor, "queue.sh eject") {
            return FAIL;
        }
    }

    let Some(hit) = hit else {
        // Not batched: a CERTIFIED or SUBMITTED bead is withdrawn the way a reopen withdraws it.
        let st = w.lc.bead_row(id);
        if !matches!(
            st.as_ref().map(|s| s.state.as_str()),
            Some("CERTIFIED" | "SUBMITTED")
        ) {
            w.err(format!("queue.sh eject: {id} is not a member of the open batch for {} and is not CERTIFIED or SUBMITTED", c.r.name));
            let ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
            w.err(format!(
                "batch members: {}",
                if ids.is_empty() {
                    "<none>".to_string()
                } else {
                    ids.join(" ")
                }
            ));
            return FAIL;
        }
        if dry_run {
            let state = st.as_ref().map(|s| s.state.clone()).unwrap_or_default();
            let tip = st.and_then(|s| s.tip).unwrap_or_default();
            w.out(format!(
                "dry-run: {id} is {state} but not yet batched for {} (tip={tip})",
                c.r.name
            ));
            w.out(format!("dry-run: would reopen bead {id}, return it to REWORK on spira-lc and clear assignee"));
            w.out(format!("dry-run: would record cause {}", cause.as_str()));
            w.out(format!("dry-run: would post comment to {id}"));
            return bead_resolves(w, id);
        }
        let reopened = w.lib.bead_reopen(id, cause.as_str(), suites);
        if !reopened {
            w.err(format!(
                "queue.sh eject: cannot reopen {id} (cause {})",
                cause.as_str()
            ));
        }
        let returned = lc_return(w, id, cause == EjectCause::EjectRed).is_ok();
        let mut comment = format!(
            "Ejected while certified but not yet batched in {}.",
            c.r.name
        );
        if !reason.is_empty() {
            comment.push_str(&format!("\n\n{reason}"));
        }
        comment
            .push_str("\n\nWithdrawn from the queue. Recertify the branch before it can rejoin.");
        if !suites.is_empty() {
            comment.push_str(&format!("\n\nRecertification will force these suites regardless of SPIRA_CERTIFY_SUITES: {suites}"));
        }
        w.lib.comment(id, &comment);
        if !reopened || !returned {
            return FAIL;
        }
        w.out(format!(
            "queue.sh eject: ejected {id} (certified, not yet batched) for {} (withdrawn)",
            c.r.name
        ));
        return OK;
    };

    if dry_run {
        w.out(format!(
            "dry-run: {id} is in the open batch for {} (tip={})",
            c.r.name, hit.tip
        ));
        w.out(format!("dry-run: would record cause {}", cause.as_str()));
        w.out(format!(
            "dry-run: would return bead {id} to spira-lc via a Returned event"
        ));
        w.out(format!("dry-run: would post comment to {id}"));
        w.out(format!("dry-run: would close PR {pr}"));
        if !survivors.is_empty() {
            let s: Vec<&str> = survivors.iter().map(|m| m.id.as_str()).collect();
            w.out(format!(
                "dry-run: would return survivors to CERTIFIED: {}",
                s.join(" ")
            ));
        }
        return bead_resolves(w, id);
    }

    let why = if reason.is_empty() {
        "ejected".to_string()
    } else {
        reason.clone()
    };
    // The cause row spira-claim classifies: eject (harness) or eject-red (judged). §8 D1.
    // `bead_reopen` carries the suites a recertification must force.
    w.lib.cause_event(id, cause.as_str());
    let mut failed = false;
    for m in &survivors {
        let _ = super::round::forget_batch_red(&c, &m.id, &m.tip);
    }
    if cause == EjectCause::EjectRed {
        if let Err(e) = super::round::record_batch_red(&c, &hit.id, &hit.tip) {
            w.err(format!("queue.sh eject: cannot bar {id}'s red tip: {e}"));
        }
    }
    w.lib.release_claim(id);
    if !batch_id.is_empty() {
        let r = bounded_text(&why);
        match lc_eject_member(w, &batch_id, id, "queue.sh", &r) {
            Ok(()) => w.out(format!("queue.sh eject: {id} ejected on spira-lc")),
            Err((rc, out)) => {
                failed = true;
                w.err(format!(
                    "queue.sh eject: spira-lc eject-member refused for {id} (rc={rc}): {out}"
                ));
            }
        }
    }
    // eject-member requeues the member to CERTIFIED, so the walk to REWORK must follow it: done
    // first, the requeue would undo it and the next cut would re-batch the bead.
    if !harness_fault || batch_id.is_empty() {
        failed |= lc_return(w, id, cause == EjectCause::EjectRed).is_err();
    }
    let mut comment = format!("Ejected from open batch in {}.", c.r.name);
    if !reason.is_empty() {
        comment.push_str(&format!("\n\n{reason}"));
    }
    comment.push_str("\n\nFix the failing issue and re-certify before rejoining the queue.");
    w.lib.comment(id, &comment);

    for m in &survivors {
        w.out(format!("queue.sh eject: {} returned to CERTIFIED", m.id));
    }
    // The PR is closed and no rebuilt one is opened: the batch row must not outlive it, or its
    // members stay IN_DELIVERY behind a PR that is gone, or an emptied batch stays OPEN.
    if !batch_id.is_empty() {
        let r = bounded_text(&format!(
            "{id} ejected; the batch PR is closed and not rebuilt"
        ));
        match lc_abandon(w, &batch_id, "queue.sh", &r) {
            Ok(()) => {
                w.out(format!(
                    "queue.sh eject: {batch_id} abandoned on spira-lc (survivors back to CERTIFIED)"
                ));
                super::round::retire_menders(w, &c, &batch_id, &format!("{id} ejected"));
            }
            Err((rc, out)) => {
                failed = true;
                w.err(format!("queue.sh eject: spira-lc abandon-batch refused for {batch_id} (rc={rc}): {out}"));
            }
        }
    }
    if failed {
        converge_orphans(w);
    }
    let branch = kv
        .as_ref()
        .and_then(|k| k.get_first("branch"))
        .unwrap_or("")
        .to_string();
    if !branch.is_empty() && idents(w, "eject", &[("branch", &branch)]).is_ok() {
        w.lib.cancel_runs(&c.s.forge, &path, &branch);
    }
    if !pr.is_empty() && idents(w, "eject", &[("pr", &pr)]).is_ok() {
        w.forge.pr_close(&c.s.forge, &path, &pr);
    }
    if let Some(o) = &open {
        if let Err(e) = o.remove(w, &c, "ejected") {
            failed = true;
            w.err(format!("queue.sh eject: the batch record ({}) could not be closed: {e}", o.place()));
        }
    }

    let surv = if survivors.is_empty() {
        "<none>".to_string()
    } else {
        crate::model::render_members(&survivors)
    };
    let mut body = format!("{id} ejected from PR {pr} by {actor}.");
    if !reason.is_empty() {
        body.push_str(&format!(" Reason: {reason}"));
    }
    body.push_str(&format!("\nSurvivors returned to CERTIFIED: {surv}"));
    w.lib.notify(
        &c.s.mailbox,
        &c.r.name,
        &format!("{id} ejected (queue.sh eject)"),
        &body,
    );
    if failed {
        w.err(format!("queue.sh eject: {id} left the {} batch record but spira-lc did not take every write — see above", c.r.name));
        return FAIL;
    }
    w.out(format!(
        "queue.sh eject: ejected {id} from {} batch {batch_id}; {} member(s) remain",
        c.r.name,
        survivors.len()
    ));
    OK
}

/// A cut that answered "cannot tell" may have written the batch row: look before retrying, so a
/// deadline neither strands BATCHED members behind no PR nor cuts the same batch twice.
#[allow(clippy::too_many_arguments)]
pub(super) fn cut_settled(
    w: &World,
    id: &str,
    repo: &str,
    head: &str,
    base: &str,
    members: &str,
    actor: &str,
    round: Option<&Kv>,
) -> Result<String, (i32, String)> {
    let mut last = Err((2, "no attempt".to_string()));
    for attempt in 0..3u32 {
        if attempt > 0 {
            w.clock.sleep(u64::from(attempt));
            if let Some((_, version)) = w.lc.batch_state(id) {
                return Ok(version);
            }
        }
        last = w.lc.cut(id, repo, head, base, members, actor, round);
        if !matches!(&last, Err((2, _))) {
            return last;
        }
    }
    if let Some((_, version)) = w.lc.batch_state(id) {
        return Ok(version);
    }
    last
}

fn bead_resolves(w: &World, id: &str) -> i32 {
    match w.bd.show(&[id.to_string()]) {
        Ok(rows) if !rows.is_empty() => OK,
        _ => {
            w.err(format!("dry-run: ERROR: cannot resolve bead {id}"));
            FAIL
        }
    }
}

pub fn abandon(w: &World, repo: Option<&str>, reason: &Text, dry_run: bool) -> i32 {
    if !czar_ok(w) {
        return FAIL;
    }
    let reason = read_text(w, reason).unwrap_or_default();
    if reason.is_empty() {
        w.err("queue.sh abandon: --reason is required — pass --reason \"<why>\" so the abandon is auditable");
        return USAGE;
    }
    let actor = actor(w);
    let Ok(c) = resolve(w, "abandon", repo) else {
        return FAIL;
    };
    let Ok(path) = repo_path(w, "abandon", &c) else {
        return FAIL;
    };
    if !dry_run && require_lc(w, "abandon").is_err() {
        return FAIL;
    }
    let Ok(_g) = take_lock(w, "abandon", &c, "") else {
        return FAIL;
    };
    abandon_locked(w, &c, &path, &reason, &actor, dry_run, "abandon")
}

/// The abandon itself, with the repo's queue lock already held by the caller.
fn abandon_locked(
    w: &World,
    c: &super::Ctx,
    path: &std::path::Path,
    reason: &str,
    actor: &str,
    dry_run: bool,
    label: &str,
) -> i32 {
    let open = match super::forge_batch::read(w, c) {
        Ok(Some(o)) => o,
        Ok(None) => {
            w.err(format!("queue.sh {label}: no open batch for {}", c.r.name));
            return FAIL;
        }
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot tell whether {} has an open batch: {e} — refused, nothing changed", c.r.name));
            return FAIL;
        }
    };
    let kv = open.kv.clone();
    if owner_refused(
        w,
        kv.get("owner").unwrap_or(""),
        actor,
        &format!("queue.sh {label}"),
    ) {
        return FAIL;
    }
    let pr = kv.get_first("pr").unwrap_or("").to_string();
    let branch = kv.get_first("branch").unwrap_or("").to_string();
    let batch_id = kv.get_first("batch_id").unwrap_or("").to_string();
    if !pr.is_empty()
        && idents(w, label, &[("pr", &pr)]).is_ok()
        && w.forge
            .pr_state(&c.s.forge, path, &pr)
            .as_deref()
            .map(str::trim)
            == Some("merged")
    {
        w.err(format!("queue.sh {label}: refused — PR {pr} is already merged; its batch lands through verdict, not abandon"));
        return FAIL;
    }
    let members = kv.members();
    for m in &members {
        let _ = super::round::forget_batch_red(c, &m.id, &m.tip);
    }
    let stamp = w.clock.stamp();
    let archive = format!("closed-pr{pr}-{stamp}");
    let reason_clean = one_line(reason);

    // A member already returned to REWORK is a verdict an abandon never overturns.
    let kept = |id: &str| -> Option<String> {
        w.lc.bead_row(id).map(|r| r.state).filter(|s| s == "REWORK")
    };

    if dry_run {
        w.out(format!("dry-run: would close PR {pr} for {}", c.r.name));
        let mut audit = Vec::new();
        for m in &members {
            match kept(&m.id) {
                Some(st) => {
                    w.out(format!("dry-run: {}: leave alone ({st})", m.id));
                    audit.push(format!("{}:{st}", m.id));
                }
                None => {
                    w.out(format!(
                        "dry-run: {}: return to CERTIFIED at {}",
                        m.id, m.tip
                    ));
                    audit.push(format!("{}:CERTIFIED", m.id));
                }
            }
        }
        let audit = if audit.is_empty() {
            "<none>".to_string()
        } else {
            audit.join(",")
        };
        w.out(format!(
            "dry-run: would cancel non-completed Gate run(s) for branch {branch}"
        ));
        match &open.at {
            super::forge_batch::At::File(_) => w.out(format!("dry-run: archive path: {}", c.queue_file(&archive).display())),
            super::forge_batch::At::Store(id) => w.out(format!("dry-run: would end batch record {id}")),
        }
        w.out(format!(
            "dry-run: audit line (landing.log): QUEUE ABANDON {} repo={} pr={pr} actor={actor} members={audit} reason={reason_clean}",
            w.clock.now(),
            c.r.name
        ));
        return OK;
    }

    if !batch_id.is_empty() {
        let r = bounded_text(reason);
        match lc_abandon(w, &batch_id, "queue.sh", &r) {
            Ok(()) => {
                w.out(format!(
                    "queue.sh {label}: {batch_id} abandoned on spira-lc"
                ));
                super::round::retire_menders(w, c, &batch_id, &r);
            }
            Err((rc, out)) => w.err(format!(
                "queue.sh {label}: spira-lc abandon-batch refused for {batch_id} (rc={rc}): {out}"
            )),
        }
    }
    if !branch.is_empty() && idents(w, label, &[("branch", &branch)]).is_ok() {
        w.lib.cancel_runs(&c.s.forge, path, &branch);
    }
    if !pr.is_empty() && idents(w, label, &[("pr", &pr)]).is_ok() {
        w.forge.pr_comment(
            &c.s.forge,
            path,
            &pr,
            &bounded_text(&format!("Batch abandoned. Reason: {reason}")),
        );
        w.forge.pr_close(&c.s.forge, path, &pr);
    }
    let mut audit = Vec::new();
    for m in &members {
        match kept(&m.id) {
            Some(st) => {
                w.out(format!("queue.sh {label}: {}: left as {st}", m.id));
                audit.push(format!("{}:{st}", m.id));
            }
            None => {
                w.out(format!("queue.sh {label}: {}: returned to CERTIFIED", m.id));
                audit.push(format!("{}:CERTIFIED", m.id));
            }
        }
    }
    let audit = if audit.is_empty() {
        "<none>".to_string()
    } else {
        audit.join(",")
    };

    // The archive keeps who and why, in the file itself.
    let mut archived = kv.clone();
    archived.push("reason", &reason_clean);
    archived.push("actor", actor);
    if let Err(e) = open.archive(w, c, &archived, &archive, "abandoned") {
        w.err(format!("queue.sh {label}: failed to archive open record: {e}"));
        return FAIL;
    }
    landing_log(
        &c.s.run,
        &format!(
            "QUEUE ABANDON {} repo={} pr={pr} actor={actor} members={audit} reason={reason_clean}",
            w.clock.now(),
            c.r.name
        ),
    );
    w.lib.event(
        "queue.abandoned",
        &format!("abandoned PR {pr} for {} (actor={actor})", c.r.name),
        &format!("members={audit} reason={reason_clean}"),
    );
    w.lib.notify(
        &c.s.mailbox,
        &c.r.name,
        "batch abandoned (queue.sh abandon)",
        &format!("PR {pr} abandoned by {actor}. Reason: {reason_clean}\nMembers: {audit}"),
    );
    w.out(format!(
        "queue.sh {label}: PR {pr} closed, batch abandoned for {}",
        c.r.name
    ));
    OK
}

/// The first prerequisite in `row`'s recorded stack, transitively, that is held, ejected at its
/// current tip, or whose row cannot be read; its dependent would carry that prerequisite's
/// commits into a batch.
fn barred_prereq(w: &World, row: &crate::model::LcBeadRow) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    let mut todo: Vec<String> = row.stack.keys().cloned().collect();
    while let Some(p) = todo.pop() {
        if !seen.insert(p.clone()) {
            continue;
        }
        match w.lc.bead_row(&p) {
            None => return Some(p),
            Some(pr)
                if !pr.holds.is_empty()
                    || (pr.ejected_red_tip.is_some() && pr.ejected_red_tip == pr.tip) =>
            {
                return Some(p)
            }
            Some(pr) => todo.extend(pr.stack.into_keys()),
        }
    }
    None
}

/// What `batch open` (and its alias `open-batch`) is asked to open.
#[derive(Debug, Clone, Copy)]
pub struct OpenArgs<'a> {
    pub repo: Option<&'a str>,
    /// `id`, `id:tip` or `id@tip`, comma- or space-separated; empty: the ranked pool.
    pub members: &'a Text,
    pub skip_pregate: bool,
    pub dry_run: bool,
    /// `--replace`, with its reason.
    pub replace: Option<&'a Text>,
    /// A head the caller already assembled and proved from exactly these members; the queue
    /// assembles one itself when it is absent.
    pub head: Option<&'a str>,
    /// The PR body; the queue writes its own when it is `Text::None`.
    pub body: &'a Text,
    /// The batch id; `<repo>-<stamp>` when absent.
    pub name: Option<&'a str>,
    /// The holder's token on `locks/round/<repo>/<name>`.
    pub lease_token: Option<&'a str>,
    /// Who the batch belongs to (its `owner`): `batcher` when the batcher opens it; the acting
    /// identity when absent.
    pub owner: Option<&'a str>,
}

/// `batch open`. With the batch machine on the control store, in four steps, each leaving the
/// record in the phase it reached, so a crash never leaves a PR the store has no record of
/// (round-controller v2 §Forge lane): 1. the batch record is written with `forge.phase = opening`
/// and its branch; 2. the branch is pushed and the PR created; 3. the record moves to `open` with
/// the PR's number, compared on `opening`; 4. auto-merge is armed and the record moves to `armed`.
/// A record left `opening` is `batch sweep`'s to adopt or release.
pub fn open_batch(w: &World, a: &OpenArgs) -> i32 {
    let OpenArgs {
        repo,
        members: members_arg,
        dry_run,
        replace,
        ..
    } = *a;
    if !czar_ok(w) {
        return FAIL;
    }
    let replace_reason = match replace {
        None => None,
        Some(t) => {
            let r = read_text(w, t).unwrap_or_default();
            if r.trim().is_empty() {
                w.err("queue.sh open-batch: --replace requires --reason \"<why>\" so the abandon is auditable");
                return USAGE;
            }
            Some(r)
        }
    };
    let members_arg = match read_text(w, members_arg) {
        Ok(m) => m,
        Err(e) => {
            w.err(format!("queue.sh open-batch: cannot read the members: {e}"));
            return USAGE;
        }
    };
    let actor = a
        .owner
        .map(str::to_string)
        .or_else(|| w.var("SPIRA_QUEUE_ACTOR"))
        .unwrap_or_else(|| "operator".into());
    let Ok(c) = resolve(w, "open-batch", repo) else {
        return FAIL;
    };
    let Ok(path) = repo_path(w, "open-batch", &c) else {
        return FAIL;
    };
    let name = c.r.name.clone();
    if c.r.mode != LandMode::Queue {
        w.err(format!(
            "queue.sh open-batch: repo is not in queue mode (mode={})",
            c.r.mode.as_str()
        ));
        return FAIL;
    }
    if !dry_run && require_lc(w, "open-batch").is_err() {
        return FAIL;
    }
    // The batcher holds the repo's lock for its whole cut and says so.
    let lock = match super::lock_held_by_caller(w) {
        true => Ok(None),
        false => take_lock(w, "open-batch", &c, ""),
    };
    let Ok(_g) = lock else {
        return FAIL;
    };
    let store = super::forge_batch::on_store(w);
    if a.lease_token.is_some() && !store {
        w.err("queue.sh open-batch: --lease-token names a lease on the control store, and the batch machine is not on it — refused, nothing changed");
        return FAIL;
    }
    if let (true, Some(id)) = (store, a.name) {
        if super::lease::lease_guard(w, "open-batch", &name, id, a.lease_token).is_err() {
            return FAIL;
        }
    }
    let read_open = || match super::forge_batch::read(w, &c) {
        Ok(o) => Ok(o),
        Err(e) => {
            w.err(format!("queue.sh open-batch: cannot tell whether {name} has an open batch: {e} — refused, nothing changed"));
            Err(FAIL)
        }
    };
    let Ok(mut open) = read_open() else {
        return FAIL;
    };
    if open.is_some() && replace_reason.is_some() && !dry_run {
        let r = replace_reason.as_deref().unwrap_or_default();
        if abandon_locked(w, &c, &path, r, &actor, false, "open-batch --replace") != OK {
            return FAIL;
        }
        let Ok(now) = read_open() else {
            return FAIL;
        };
        open = now;
    }
    if open.is_some() && !(dry_run && replace_reason.is_some()) {
        let owner = open
            .as_ref()
            .and_then(|o| o.kv.get("owner").map(String::from))
            .unwrap_or_default();
        let owner = if owner.is_empty() {
            "batcher".into()
        } else {
            owner
        };
        w.err(format!("queue.sh open-batch: a batch is already open for {name} (owner={owner}) — abandon it first (queue.sh abandon) or pass --replace --reason"));
        return FAIL;
    }
    if !(dry_run && replace_reason.is_some()) {
        if let Some((pr, br)) = w
            .forge
            .queue_prs(&c.s.forge, &path)
            .and_then(|v| v.into_iter().next())
        {
            w.err(format!("queue.sh open-batch: {name} already has open batch PR {pr} ({br}) on the forge — nothing cut; abandon it first (queue.sh abandon) or pass --replace --reason"));
            return FAIL;
        }
    }
    if let Some(until) = records::read_kv(&c.queue_file("capacity-backoff"))
        .ok()
        .flatten()
        .and_then(|k| k.get("until").and_then(|u| u.trim().parse::<u64>().ok()))
    {
        let now = w.clock.now();
        if until > now && !dry_run {
            w.out(format!("queue.sh open-batch: no cut — {name} is waiting on capacity (a capacity failure closed its last batch); {}s of backoff remain", until - now));
            return FAIL;
        }
    }
    let Some(base) = c.r.landref.clone() else {
        w.err(format!(
            "queue.sh open-batch: cannot resolve landing ref for {name}"
        ));
        return FAIL;
    };
    let Some(base_sha) = w.git.rev_parse(&path, &base) else {
        w.err(format!("queue.sh open-batch: cannot resolve {base}"));
        return FAIL;
    };
    let branches = w.git.branches(&path, "refs/heads/spira/");
    let certs = super::helpers::certified_rows(w.lc, &branches);
    let submitted = super::helpers::state_rows(w.lc, &branches, "SUBMITTED");
    let mut skips: Vec<String> = Vec::new();
    let mut cands: Vec<(String, String)> = Vec::new();
    if !members_arg.trim().is_empty() {
        for spec in members_arg
            .split(|ch: char| ch == ',' || ch.is_whitespace())
            .filter(|s| !s.is_empty())
        {
            let (mid, pin) = match spec.split_once(['@', ':']) {
                Some((m, t)) => (m, Some(t)),
                None => (spec, None),
            };
            match certs
                .iter()
                .chain(submitted.iter())
                .find(|(i, _, _)| i == mid)
            {
                Some((_, t, _)) if pin.is_some_and(|p| p != t) => skips.push(format!(
                    "{mid}: named at {}, but its tip is {t} — not admitted",
                    pin.unwrap_or_default()
                )),
                Some((i, t, _)) => cands.push((i.clone(), t.clone())),
                None => skips.push(format!("{mid}: not SUBMITTED or CERTIFIED")),
            }
        }
    } else {
        let express: std::collections::HashSet<String> = match w.lc.express_ids() {
            Ok(ids) => ids.into_iter().collect(),
            Err(e) => {
                eprintln!("queue: express set unreadable ({e}) -- ranking without it");
                Default::default()
            }
        };
        for group in [&certs, &submitted] {
            if group.is_empty() {
                continue;
            }
            let ids: Vec<String> = group.iter().map(|(i, _, _)| i.clone()).collect();
            let prio =
                w.bd.show(&ids)
                    .ok()
                    .map(|rows| rows_json(&rows))
                    .unwrap_or_else(|| "[]".into());
            let rows: String = group.iter().fold(String::new(), |mut acc, (i, t, e)| {
                acc.push_str(&format!("{i} {t} {e}\n"));
                acc
            });
            cands.extend(w.lib.sort_rows(&express, &path, &base_sha, &prio, &rows));
        }
    }

    // Admission: the member's lifecycle row, read fresh, is still CERTIFIED or SUBMITTED — waiting
    // for a round; a SUBMITTED member is provisional, and its gate red ejects it (verdict). Never bd's status (design §3.4, sp-mve9i: bd status is inert for work beads, and
    // the work verbs never move it). A row the machine cannot read is not admitted
    // (DESIGN.md §8 D8).
    let mut admitted = Vec::new();
    let mut provisional: Vec<String> = Vec::new();
    for (id, tip) in cands {
        match w.lc.bead_row(&id) {
            None => skips.push(format!(
                "{id}: no lifecycle row (spira-lc could not say) — not admitted"
            )),
            Some(r)
                if matches!(r.state.as_str(), "CERTIFIED" | "SUBMITTED")
                    && super::round::batch_red_at(&c, &id, &tip) =>
            {
                skips.push(format!(
                    "{id}: {tip} already went red in a batch — not admitted until the tip changes"
                ))
            }
            Some(r)
                if matches!(r.state.as_str(), "CERTIFIED" | "SUBMITTED") && !r.holds.is_empty() =>
            {
                skips.push(format!("{id}: held ({}) — not admitted", r.holds.join(",")))
            }
            Some(r) if matches!(r.state.as_str(), "CERTIFIED" | "SUBMITTED") => {
                match barred_prereq(w, &r) {
                    Some(p) => skips.push(format!("{id}: stacked on held {p} — not admitted")),
                    None => {
                        if r.state == "SUBMITTED" {
                            if let Some(why) = super::round::gate_red_at(&c, &id, &tip) {
                                skips.push(format!(
                                    "{id}: its gate is FAIL at {tip} ({why}) — not admitted"
                                ));
                                continue;
                            }
                            provisional.push(id.clone());
                        }
                        admitted.push((id, tip))
                    }
                }
            }
            Some(r) => skips.push(format!(
                "{id}: lifecycle state={} (no longer SUBMITTED or CERTIFIED) — not admitted",
                r.state
            )),
        }
    }
    super::stage::refuse_over_cap(&mut admitted, &mut skips, |(id, _)| id);

    if let Some(given) = a.head {
        // The caller's head carries every member it names: one left out is a head that is not
        // the batch's, so nothing is cut.
        for s in &skips {
            w.out(format!("queue.sh open-batch: skip — {s}"));
        }
        if !skips.is_empty() || admitted.is_empty() {
            w.err(format!("queue.sh open-batch: no cut — {given} is the head of every member named, and {} of them {} not admitted for {name}", skips.len().max(1), if skips.len() > 1 { "were" } else { "was" }));
            return FAIL;
        }
        let Some(head) = w.git.rev_parse(&path, given) else {
            w.err(format!("queue.sh open-batch: cannot resolve the head {given} — nothing cut"));
            return FAIL;
        };
        let members: Vec<Member> = admitted.into_iter().map(|(id, tip)| Member { id, tip }).collect();
        let provisional = provisional.join(" ");
        return publish(w, &c, &path, a, &members, &head, None, &base_sha, &actor, &provisional);
    }
    let wt =
        c.s.run
            .join("worktree")
            .join(format!(".open-batch-{name}-{}", w.env.pid()));
    w.git.worktree_prune(&path);
    if let Some(p) = wt.parent() {
        let _ = fs::create_dir_all(p);
    }
    if !w.git.worktree_add_detached(&path, &wt, &base_sha) {
        w.err("queue.sh open-batch: cannot create assembly worktree");
        return FAIL;
    }
    let mut members: Vec<Member> = Vec::new();
    for (id, tip) in admitted {
        if crate::ident::check("member", &id).is_err() || crate::ident::check("tip", &tip).is_err()
        {
            skips.push(format!("{id}: not a valid id:tip"));
            continue;
        }
        if !super::simple::lint_passed(w, "open-batch", &path, &tip, false) {
            skips.push(format!(
                "{id}: {tip} fails its compile-and-lint check — not admitted"
            ));
            continue;
        }
        if super::round::merge_member(w, &c, &wt, &id, &tip) {
            members.push(Member { id, tip });
        } else {
            w.git.merge_abort(&wt);
            let why = if w.lib.base_conflict(&path, &base_sha, &tip) {
                "conflicts with base"
            } else {
                "conflicts with batch"
            };
            skips.push(format!("{id}: {why}"));
        }
    }
    for s in &skips {
        w.out(format!("queue.sh open-batch: skip — {s}"));
    }
    if members.is_empty() {
        w.out(format!(
            "queue.sh open-batch: no cut — nothing admissible for {name}"
        ));
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }
    w.lib.format_batch(&wt, &base_sha, &name);
    let head = w.git.rev_parse(&wt, "HEAD").unwrap_or_default();
    let provisional = provisional
        .iter()
        .filter(|p| members.iter().any(|m| &m.id == *p))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    publish(w, &c, &path, a, &members, &head, Some(&wt), &base_sha, &actor, &provisional)
}

/// The rest of `batch open` once its members and head are settled: the dry run, the forge's
/// preflight and the pre-flight gate, then the batch's record, branch and PR.
#[allow(clippy::too_many_arguments)]
fn publish(
    w: &World,
    c: &super::Ctx,
    path: &std::path::Path,
    a: &OpenArgs,
    members: &[Member],
    head: &str,
    wt: Option<&std::path::Path>,
    base_sha: &str,
    actor: &str,
    provisional: &str,
) -> i32 {
    let name = c.r.name.as_str();
    let drop_wt = || {
        if let Some(wt) = wt {
            w.git.worktree_remove(path, wt);
        }
    };
    let base = c.r.landref.clone().unwrap_or_default();
    let remote = c.r.ref_remote(&base).unwrap_or_else(|| "origin".into());
    let base_branch = ref_branch(&base).to_string();
    let stamp = w.clock.stamp();
    let batch_br = format!("spira/queue/{stamp}");
    let rendered = crate::model::render_members(members);

    if a.dry_run {
        w.out(format!("queue.sh open-batch: dry-run for {name}"));
        w.out("members:");
        for m in members {
            w.out(format!("  {}", m.render()));
        }
        w.out(format!("merge head: {head}"));
        w.out("would write open record:");
        w.out("  pr=<pending>");
        w.out(format!("  head={head}"));
        w.out(format!("  base={base_sha}"));
        w.out(format!("  members={rendered}"));
        w.out("  opened=<pending>");
        w.out(format!("  branch={batch_br}"));
        w.out(format!("  owner={actor}"));
        drop_wt();
        return OK;
    }

    if let Err(e) = w.forge.pr_preflight(&c.s.forge, path, &batch_br) {
        w.err(format!(
            "queue.sh open-batch: forge refuses {batch_br}: {e} — nothing pushed"
        ));
        drop_wt();
        return FAIL;
    }
    w.git.branch_set(path, &batch_br, head, true);
    drop_wt();

    if a.skip_pregate {
        w.out(
            "queue.sh open-batch: pre-flight gate skipped (--skip-pregate) — CI is the authority",
        );
    } else {
        let (mut rc, out) = w
            .lib
            .pf_gate(&batch_br, name, &stamp, c.s.preflight_wall_secs);
        if rc == 124 {
            w.out("queue.sh open-batch: pre-flight hit its wall — opening the PR, CI decides");
            rc = 0;
        }
        if rc != 0 {
            w.err(format!("queue.sh open-batch: local pre-flight gate failed for {batch_br} — not opening a batch"));
            w.err(out.trim_end_matches('\n'));
            w.err("queue.sh open-batch: retry with --skip-pregate to let CI be the gate");
            w.git.branch_delete(path, &batch_br);
            return FAIL;
        }
    }
    let body = match a.body {
        Text::None => {
            let ids: Vec<String> = members.iter().map(|m| m.id.clone()).collect();
            let titles = w.bd.show(&ids).unwrap_or_default();
            let mut body = format!(
                "Merge-queue batch: {} beads for {name}, onto {base_branch}.\n\n",
                members.len()
            );
            if a.skip_pregate {
                body.push_str("Opened with --skip-pregate: the local pre-flight gate was not run for this batch; CI is the gate.\n\n");
            }
            for id in &ids {
                body.push_str(&format!("- {id} — {}\n", title_line(&titles, id)));
            }
            body
        }
        given => match read_text(w, given) {
            Ok(b) => b,
            Err(e) => {
                w.err(format!("queue.sh open-batch: cannot read the PR body: {e} — nothing pushed"));
                w.git.branch_delete(path, &batch_br);
                return FAIL;
            }
        },
    };
    let lc_id = a.name.map_or_else(|| format!("{name}-{stamp}"), str::to_string);
    let csv = members
        .iter()
        .map(Member::render)
        .collect::<Vec<_>>()
        .join(",");
    for m in members {
        w.lc.create_bead(&m.id);
    }
    let opened = w.clock.now();
    let mut rec = Kv::default();
    for (k, v) in [
        ("head", head),
        ("base", base_sha),
        ("members", rendered.as_str()),
        ("opened", &opened.to_string()),
        ("branch", batch_br.as_str()),
        ("owner", actor),
    ] {
        rec.push(k, v);
    }
    if !provisional.is_empty() {
        rec.push("provisional", provisional);
        w.out(format!(
            "queue.sh open-batch: provisional (SUBMITTED, gate still running): {provisional}"
        ));
    }
    let title = format!("queue: {} beads for {name}", members.len());
    let pr_req = PrRequest {
        path,
        remote: &remote,
        head,
        branch: &batch_br,
        base_branch: &base_branch,
        title: &title,
        body: body.trim_end_matches('\n'),
    };
    let opened_pr = if super::forge_batch::on_store(w) {
        open_on_store(w, c, &pr_req, &lc_id, &csv, base_sha, rec)
    } else {
        open_with_file(w, c, &pr_req, &lc_id, &csv, base_sha, rec)
    };
    let Ok((pr, armed)) = opened_pr else {
        return FAIL;
    };
    let _ = fs::remove_file(c.s.run.join(format!("queue-stuck-{name}")));
    landing_log(&c.s.run, &format!("QUEUE BATCH {opened} repo={name} members={} gate_seconds=0 verdict=green source=open-batch", members.len()));
    w.out(format!(
        "queue.sh open-batch: PR {pr} opened — {} branches ({batch_br})",
        members.len()
    ));
    // For a caller that records them: never read off the line above.
    w.out(format!("batch_id={lc_id}"));
    w.out(format!("pr={pr}"));
    if armed {
        OK
    } else {
        FAIL
    }
}

/// The batch PR `batch open` asks the forge for.
struct PrRequest<'a> {
    path: &'a std::path::Path,
    remote: &'a str,
    head: &'a str,
    branch: &'a str,
    base_branch: &'a str,
    title: &'a str,
    body: &'a str,
}

impl PrRequest<'_> {
    /// Pushes the branch and creates its PR: the PR's number, or why there is none. A create the
    /// forge reports failed while a PR is open on the branch is that PR.
    fn push_and_create(&self, w: &World, c: &super::Ctx) -> Result<String, String> {
        let spec = format!("{}:refs/heads/{}", self.head, self.branch);
        if !w.lib.push(self.path, self.remote, &spec) {
            return Err(format!("could not push {}", self.branch));
        }
        let pr = match w.forge.pr_create(&c.s.forge, self.path, self.branch, self.base_branch, self.title, self.body) {
            Ok(pr) => pr,
            Err(e) => {
                let on_branch = w.forge.queue_prs(&c.s.forge, self.path).and_then(|prs| {
                    prs.into_iter().find(|(_, b)| b == self.branch).map(|(n, _)| n)
                });
                match on_branch {
                    Some(n) => {
                        w.err(format!("queue.sh open-batch: forge pr-create reported failure for {} ({e}) but PR {n} is open on it — adopting it", self.branch));
                        n
                    }
                    None => return Err(format!("forge pr-create failed for {}: {e}", self.branch)),
                }
            }
        };
        if pr.is_empty() {
            return Err(format!("forge returned no PR number for {}", self.branch));
        }
        Ok(pr)
    }

    fn arm(&self, w: &World, c: &super::Ctx, pr: &str) -> bool {
        let armed = w.forge.pr_automerge(&c.s.forge, self.path, pr);
        if !armed {
            w.err(format!("queue.sh open-batch: PR {pr} for {} is open but auto-merge could not be armed — the reconciler arms it on the next pass", c.r.name));
        }
        armed
    }
}

/// `batch open` with the batch machine on the control store: the record first, then the PR, so
/// no step leaves a PR the store holds no record of. Ok((PR, armed)).
fn open_on_store(
    w: &World,
    c: &super::Ctx,
    p: &PrRequest,
    lc_id: &str,
    csv: &str,
    base_sha: &str,
    mut rec: Kv,
) -> Result<(String, bool), ()> {
    use super::forge_batch::{ARMED, OPEN, OPENING, PHASE};
    let name = c.r.name.as_str();
    rec.push(PHASE, OPENING);
    if let Err((rc, out)) = cut_settled(w, lc_id, name, p.head, base_sha, csv, "queue.sh", Some(&rec)) {
        w.err(format!("queue.sh open-batch: spira-lc cut refused for {lc_id} (rc={rc}): {out} — no PR opened"));
        w.git.branch_delete(p.path, p.branch);
        return Err(());
    }
    w.out(format!("queue.sh open-batch: {lc_id} recorded on the control store (forge.phase=opening)"));
    let left_opening = |why: &str| {
        w.err(format!("queue.sh open-batch: {why} — batch {lc_id} stays forge.phase=opening; `queue batch sweep --repo {name}` adopts its PR or releases it once its lease TTL has passed"));
    };
    let pr = match p.push_and_create(w, c) {
        Ok(pr) => pr,
        Err(e) => {
            left_opening(&e);
            return Err(());
        }
    };
    let mut kv = rec;
    kv.lines.insert(0, ("batch_id".into(), lc_id.into()));
    kv.lines.insert(1, ("pr".into(), pr.clone()));
    let mut step = |from: &str, to: &str| -> Result<(), String> {
        kv.remove(PHASE);
        kv.push(PHASE, to);
        match w.lc.save_batch_if(name, &kv, PHASE, from) {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!("batch {lc_id} left forge.phase={from} before this open moved it to {to} (a sweep took it on)")),
            Err(e) => Err(format!("the move of batch {lc_id} to forge.phase={to} could not be written: {e}")),
        }
    };
    if let Err(e) = step(OPENING, OPEN) {
        left_opening(&format!("PR {pr} is open but {e}"));
        return Err(());
    }
    let armed = p.arm(w, c, &pr);
    if armed {
        if let Err(e) = step(OPEN, ARMED) {
            w.err(format!("queue.sh open-batch: PR {pr} is armed but {e}"));
        }
    }
    Ok((pr, armed))
}

/// `batch open` while the open batch is the `open` file: the batch is cut, its PR opened and armed,
/// then the file written.
fn open_with_file(
    w: &World,
    c: &super::Ctx,
    p: &PrRequest,
    lc_id: &str,
    csv: &str,
    base_sha: &str,
    rec: Kv,
) -> Result<(String, bool), ()> {
    let open = c.queue_file("open");
    let version = match cut_settled(w, lc_id, &c.r.name, p.head, base_sha, csv, "queue.sh", None) {
        Ok(v) => v,
        Err((rc, out)) => {
            w.err(format!("queue.sh open-batch: spira-lc cut refused for {lc_id} (rc={rc}): {out} — no PR opened"));
            w.git.branch_delete(p.path, p.branch);
            return Err(());
        }
    };
    let undo = |why: &str| {
        let _ = lc_abandon(w, lc_id, "queue.sh", why);
        w.git.branch_delete(p.path, p.branch);
    };
    let pr = match p.push_and_create(w, c) {
        Ok(pr) => pr,
        Err(e) => {
            w.err(format!("queue.sh open-batch: {e}"));
            undo("open-batch could not open the batch PR");
            return Err(());
        }
    };
    let armed = p.arm(w, c, &pr);
    let mut rec = rec;
    rec.lines.insert(0, ("pr".into(), pr.clone()));
    if let Err(e) = write_atomic(&open, &rec.render()) {
        w.err(format!("queue.sh open-batch: {e}"));
        undo("open-batch could not write the open record");
        return Err(());
    }
    if !version.is_empty() {
        rec.push("batch_id", lc_id);
        rec.push("version", &version);
        let _ = write_atomic(&open, &rec.render());
        w.out(format!(
            "queue.sh open-batch: {lc_id} cut on spira-lc (version={version})"
        ));
    }
    Ok((pr, armed))
}

/// `batch show`: the open batch's record as `key=value` lines, whatever its phase, and nothing when
/// the repo has none. Exit 1 when the record cannot be read: no answer is never "none open".
pub fn batch_show(w: &World, repo: Option<&str>) -> i32 {
    let Ok(c) = resolve(w, "batch show", repo) else {
        return FAIL;
    };
    match super::forge_batch::read(w, &c) {
        Ok(Some(o)) => {
            w.io.out(&o.kv.render());
            OK
        }
        Ok(None) => OK,
        Err(e) => {
            w.err(format!("queue.sh batch show: cannot tell whether {} has an open batch: {e}", c.r.name));
            FAIL
        }
    }
}

/// The config key whose value is one lease's TTL: a `batch open` that has held its record at
/// `opening` for longer has crashed or been abandoned.
const LEASE_TTL_KEY: &str = "SPIRA_ROUND_LEASE_TTL_SECS";

/// `batch sweep`: settles what a crashed `batch open` left behind. A `spira/queue/*` PR no batch
/// record names is closed and its branch deleted; a record still `opening` past one lease TTL, with
/// no live lease, is adopted (moved to `open`) when its PR exists and released (its batch abandoned)
/// when it does not. It never merges anything.
pub fn batch_sweep(w: &World, repo: Option<&str>) -> i32 {
    const LABEL: &str = "batch sweep";
    if !czar_ok(w) {
        return FAIL;
    }
    let Ok(c) = resolve(w, LABEL, repo) else {
        return FAIL;
    };
    let Ok(path) = repo_path(w, LABEL, &c) else {
        return FAIL;
    };
    if c.r.mode != LandMode::Queue {
        w.err(format!(
            "queue.sh {LABEL}: repo is not in queue mode (mode={}) — nothing swept",
            c.r.mode.as_str()
        ));
        return FAIL;
    }
    if !super::forge_batch::on_store(w) {
        w.err(format!("queue.sh {LABEL}: the sweep reads the batch records on the control store, and the batch machine is not on it (SPIRA_LC_LIFECYCLE_ON_STORE=0) — nothing swept"));
        return FAIL;
    }
    if require_lc(w, LABEL).is_err() {
        return FAIL;
    }
    let Ok(_g) = take_lock(w, LABEL, &c, "") else {
        return FAIL;
    };
    sweep_locked(w, &c, &path)
}

/// The sweep itself, with the repo's queue lock held by the caller.
pub(super) fn sweep_locked(w: &World, c: &super::Ctx, path: &std::path::Path) -> i32 {
    use super::forge_batch::{OPEN, OPENING, PHASE};
    const LABEL: &str = "batch sweep";
    let name = c.r.name.as_str();
    // The batch the queue kept before the store is no record the sweep reads: its PR would be
    // closed as one no record names.
    let legacy = c.queue_file("open");
    if legacy.is_file() {
        w.err(format!("queue.sh {LABEL}: {} still holds a batch recorded before the batch machine moved to the control store — nothing swept; it is settled through it (`queue verdict {name}`, or `queue abandon {name}`), which removes it", legacy.display()));
        return FAIL;
    }
    let ttl = match w.env.config(LEASE_TTL_KEY).map(|v| v.trim().parse::<u64>()) {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => {
            w.err(format!("queue.sh {LABEL}: {LEASE_TTL_KEY} does not parse as seconds ({e}) — nothing swept"));
            return FAIL;
        }
        Err(e) => {
            w.err(format!("queue.sh {LABEL}: {LEASE_TTL_KEY} does not resolve ({e}) — nothing swept"));
            return FAIL;
        }
    };
    let live = match super::forge_batch::live(w, c) {
        Ok(l) => l,
        Err(e) => {
            w.err(format!("queue.sh {LABEL}: cannot tell which batches {name} has open: {e} — nothing swept"));
            return FAIL;
        }
    };
    let Some(prs) = w.forge.queue_prs(&c.s.forge, path) else {
        w.err(format!("queue.sh {LABEL}: the forge cannot list {name}'s spira/queue/* PRs — nothing swept"));
        return FAIL;
    };
    let mut rc = OK;
    for (pr, branch) in &prs {
        if live.iter().any(|b| b.kv.get("branch") == Some(branch.as_str())) {
            continue;
        }
        if idents(w, LABEL, &[("pr", pr), ("branch", branch)]).is_err() {
            rc = FAIL;
            continue;
        }
        w.out(format!("queue.sh {LABEL}: closing PR {pr} ({branch}) — no batch record of {name} names it; it is never merged"));
        w.forge.pr_close(&c.s.forge, path, pr);
        w.lib.cancel_runs(&c.s.forge, path, branch);
        delete_branch(w, c, path, branch);
    }
    let now = w.clock.now();
    for b in live.iter().filter(|b| b.kv.get(PHASE) == Some(OPENING)) {
        let age = now.saturating_sub(b.kv.get("opened").and_then(|o| o.parse().ok()).unwrap_or(0));
        if age < ttl {
            w.out(format!("queue.sh {LABEL}: batch {} has been opening for {age}s, inside one lease TTL ({ttl}s) — left to its opener", b.id));
            continue;
        }
        match w.lc.round_lease(name, &b.id) {
            Err(e) => {
                w.err(format!("queue.sh {LABEL}: cannot tell whether batch {} is leased: {e} — left as it is", b.id));
                rc = FAIL;
                continue;
            }
            Ok(Some(l)) => {
                let holder = l.holder.as_deref().unwrap_or("a holder no record names");
                w.out(format!("queue.sh {LABEL}: batch {} is opening under {holder}'s live lease (token {}) — left to its holder", b.id, l.token));
                continue;
            }
            Ok(None) => {}
        }
        let branch = b.kv.get("branch").unwrap_or("");
        match prs.iter().find(|(_, br)| br == branch) {
            Some((pr, _)) => {
                let mut kv = b.kv.clone();
                kv.remove("pr");
                kv.push("pr", pr);
                kv.remove(PHASE);
                kv.push(PHASE, OPEN);
                match w.lc.save_batch_if(name, &kv, PHASE, OPENING) {
                    Ok(true) => w.out(format!("queue.sh {LABEL}: adopted batch {} — its PR {pr} ({branch}) exists, forge.phase=open", b.id)),
                    Ok(false) => w.out(format!("queue.sh {LABEL}: batch {} left forge.phase=opening while the sweep read it — left as it is", b.id)),
                    Err(e) => {
                        w.err(format!("queue.sh {LABEL}: batch {}'s adoption of PR {pr} could not be written: {e}", b.id));
                        rc = FAIL;
                    }
                }
            }
            None => {
                let why = bounded_text(&format!("batch open left {} opening for {age}s with no PR on {branch}", b.id));
                match lc_abandon(w, &b.id, "queue.sh", &why) {
                    Ok(()) => {
                        w.out(format!("queue.sh {LABEL}: released batch {} — no PR exists on {branch}; its members go back to the pool", b.id));
                        if !branch.is_empty() && idents(w, LABEL, &[("branch", branch)]).is_ok() {
                            w.git.branch_delete_sanctioned(path, branch);
                        }
                    }
                    Err((code, out)) => {
                        w.err(format!("queue.sh {LABEL}: the release of batch {} was refused (rc={code}): {out}", b.id));
                        rc = FAIL;
                    }
                }
            }
        }
    }
    rc
}

/// Deletes a batch branch on the landing remote and here.
fn delete_branch(w: &World, c: &super::Ctx, path: &std::path::Path, branch: &str) {
    if let Some(remote) = c.r.landref.as_deref().and_then(|b| c.r.ref_remote(b)) {
        w.lib.push(path, &remote, &format!(":refs/heads/{branch}"));
    }
    w.git.branch_delete_sanctioned(path, branch);
}

/// The bd rows as the JSON array `queue_sort_rows` reads through PRIO_JSON.
fn rows_json(rows: &[crate::model::BeadRow]) -> String {
    let v: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| serde_json::json!({"id": r.id, "title": r.title, "status": r.status, "labels": r.labels, "priority": r.priority}))
        .collect();
    serde_json::Value::Array(v).to_string()
}
