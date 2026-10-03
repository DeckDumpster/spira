//! The queue.forge open batch: eject, abandon, open-batch (DESIGN.md §2.2).

use std::fs;

use super::{actor, czar_ok, lifecycle_on, require_lc, idents, landing_log, owner_refused, read_text, repo_path, resolve, take_lock, title_line, World, FAIL, OK, USAGE};
use crate::cli::Text;
use crate::ident::bounded_text;
use crate::model::{EjectCause, LandMode, Member};
use crate::ports::ref_branch;
use crate::records::{self, one_line, write_atomic, Kv};

/// Ask spira-lc for the batch's fresh (state, version) and run `f` against it; rc 1 with no
/// output when the batch row does not exist there (queue.sh _lc_batch_state_version).
fn lc_cas<F>(w: &World, batch_id: &str, f: F) -> Result<(), (i32, String)>
where
    F: FnOnce(&str, &str) -> Result<(), (i32, String)>,
{
    if !w.lc.available() {
        return Err((2, "no spira-lc program".into()));
    }
    match w.lc.batch_state(batch_id) {
        Some((state, version)) if !state.is_empty() => f(&state, &version),
        _ => Err((1, String::new())),
    }
}

/// A hand eject's walk on spira-lc: Deliver first when the row is still CERTIFIED (Returned is
/// legal only from IN_DELIVERY), then Returned{batch-ejected} -> REWORK. Reported, never fatal:
/// the landstate is already written.
fn lc_return(w: &World, id: &str) {
    let fail = |why: String| w.err(format!("queue.sh eject: spira-lc: {id}: {why} — not returned to REWORK on spira-lc"));
    let Some((mut state, version)) = w.lc.bead_state(id) else { return fail("no lifecycle row".into()) };
    let Ok(mut v) = version.trim().parse::<u64>() else { return fail(format!("unreadable version {version:?}")) };
    let who = "queue.sh";
    if state == "CERTIFIED" {
        if let Err((rc, e)) = w.lc.bead_event(id, &state, &v.to_string(), who, "\"Deliver\"") {
            return fail(format!("Deliver refused (rc={rc}): {e}"));
        }
        state = "IN_DELIVERY".into();
        v += 1;
    }
    if state != "IN_DELIVERY" {
        return fail(format!("in state {state}, not CERTIFIED or IN_DELIVERY"));
    }
    if let Err((rc, e)) = w.lc.bead_event(id, &state, &v.to_string(), who, "{\"Returned\":{\"reason\":\"batch-ejected\"}}") {
        fail(format!("Returned refused (rc={rc}): {e}"));
    }
}

#[allow(clippy::too_many_arguments)]
pub fn eject(w: &World, id: &str, repo: Option<&str>, reason: &Text, suites: &str, red: bool, dry_run: bool) -> i32 {
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
    let Ok(c) = resolve(w, "eject", repo) else { return FAIL };
    let Ok(path) = repo_path(w, "eject", &c) else { return FAIL };
    if idents(w, "eject", &[("bead id", id), ("repo", &c.r.name)]).is_err() {
        return FAIL;
    }
    // --suites is a comma-separated list (queue.sh's contract, and what the .ejected sidecar
    // and recertification read back): each name is an identifier, the commas are not.
    if !suites.is_empty() && suites.split(',').try_for_each(|s| idents(w, "eject", &[("suite", s)])).is_err() {
        return FAIL;
    }
    let actor = actor(w);
    let cause = EjectCause::decide(red, suites);
    let lc_on = lifecycle_on(w);
    if lc_on && !dry_run && require_lc(w, "eject").is_err() {
        return FAIL;
    }
    let Ok(_g) = take_lock(w, "eject", &c, "") else { return FAIL };

    let open = c.queue_file("open");
    let kv = records::read_kv(&open).ok().flatten();
    let members = kv.as_ref().map(Kv::members).unwrap_or_default();
    let pr = kv.as_ref().and_then(|k| k.get_first("pr")).unwrap_or("").to_string();
    let batch_id = kv.as_ref().and_then(|k| k.get_first("batch_id")).unwrap_or("").to_string();
    let hit = members.iter().find(|m| m.id == id).cloned();
    let survivors: Vec<Member> = members.iter().filter(|m| m.id != id).cloned().collect();

    if hit.is_some() {
        let owner = kv.as_ref().and_then(|k| k.get("owner")).unwrap_or("");
        if owner_refused(w, owner, &actor, "queue.sh eject") {
            return FAIL;
        }
    }
    let landstate = &c.s.landstate;

    let Some(hit) = hit else {
        // Not batched: a CERTIFIED bead is withdrawn the way a reopen withdraws it.
        let st = records::land_state(landstate, id);
        if st.as_ref().map(|s| s.state.as_str()) != Some("CERTIFIED") {
            w.err(format!("queue.sh eject: {id} is not a member of the open batch for {} and is not CERTIFIED", c.r.name));
            let ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
            w.err(format!("batch members: {}", if ids.is_empty() { "<none>".to_string() } else { ids.join(" ") }));
            return FAIL;
        }
        if dry_run {
            let tip = st.map(|s| s.tip).unwrap_or_default();
            w.out(format!("dry-run: {id} is CERTIFIED but not yet batched for {} (tip={tip})", c.r.name));
            w.out(format!("dry-run: would write WITHDRAWN to {}/{id}", landstate.display()));
            if !suites.is_empty() {
                w.out(format!("dry-run: would write suites={suites} to {}/{id}.ejected", landstate.display()));
            }
            w.out(format!("dry-run: would reopen bead {id} and clear assignee"));
            w.out(format!("dry-run: would record cause {}", cause.as_str()));
            w.out(format!("dry-run: would post comment to {id}"));
            return bead_resolves(w, id);
        }
        w.lib.bead_reopen(id, cause.as_str(), suites);
        if lc_on {
            lc_return(w, id);
        }
        let mut comment = format!("Ejected while certified but not yet batched in {}.", c.r.name);
        if !reason.is_empty() {
            comment.push_str(&format!("\n\n{reason}"));
        }
        comment.push_str("\n\nLandstate written as WITHDRAWN. Recertify the branch before it can rejoin the queue.");
        if !suites.is_empty() {
            comment.push_str(&format!("\n\nRecertification will force these suites regardless of SPIRA_CERTIFY_SUITES: {suites}"));
        }
        w.lib.comment(id, &comment);
        w.out(format!("queue.sh eject: ejected {id} (certified, not yet batched) for {} (landstate=WITHDRAWN)", c.r.name));
        return OK;
    };

    if dry_run {
        w.out(format!("dry-run: {id} is in the open batch for {} (tip={})", c.r.name, hit.tip));
        w.out(format!("dry-run: would write RED to {}/{id}", landstate.display()));
        if !suites.is_empty() {
            w.out(format!("dry-run: would write suites={suites} to {}/{id}.ejected", landstate.display()));
        }
        w.out(format!("dry-run: would record cause {}", cause.as_str()));
        if lc_on {
            w.out(format!("dry-run: would return bead {id} to spira-lc via a Returned event"));
        } else {
            w.out(format!("dry-run: would reopen bead {id} and clear assignee"));
        }
        w.out(format!("dry-run: would post comment to {id}"));
        w.out(format!("dry-run: would close PR {pr}"));
        if !survivors.is_empty() {
            let s: Vec<&str> = survivors.iter().map(|m| m.id.as_str()).collect();
            w.out(format!("dry-run: would return survivors to CERTIFIED: {}", s.join(" ")));
        }
        return bead_resolves(w, id);
    }

    let why = if reason.is_empty() { "ejected".to_string() } else { reason.clone() };
    // RED, not EJECTED: EJECTED is the automated attribution state; RED is the operator's.
    w.lib.land_mark(id, "RED", &hit.tip, &why);
    if lc_on {
        // The suites the dry-run promised (queue.sh printed this line and never wrote it).
        if !suites.is_empty() {
            let _ = write_atomic(&landstate.join(format!("{id}.ejected")), suites);
        }
        // The cause row spira-claim classifies: eject (harness) or eject-red (judged). §8 D1.
        w.lib.cause_event(id, cause.as_str());
        // The delivery-exit event legal from IN_DELIVERY (sp-rlyl0).
        lc_return(w, id);
        w.lib.release_claim(id);
    } else {
        // Pre-lifecycle (before sp-rlyl0): hand the bead back through bead_reopen — reopen,
        // submitted label off, assignee cleared, the suites sidecar and the cause row, in
        // the one function every reopen goes through.
        w.lib.bead_reopen(id, cause.as_str(), suites);
    }
    if lc_on && !batch_id.is_empty() {
        let r = bounded_text(&why);
        match lc_cas(w, &batch_id, |s, v| w.lc.eject_member(&batch_id, id, s, v, "queue.sh", &r)) {
            Ok(()) => w.out(format!("queue.sh eject: {id} ejected on spira-lc (returned to CERTIFIED there)")),
            Err((rc, out)) => w.err(format!("queue.sh eject: spira-lc eject-member refused for {id} (rc={rc}): {out}")),
        }
    }
    let mut comment = format!("Ejected from open batch in {}.", c.r.name);
    if !reason.is_empty() {
        comment.push_str(&format!("\n\n{reason}"));
    }
    comment.push_str("\n\nLandstate written as RED. Fix the failing issue and re-certify before rejoining the queue.");
    w.lib.comment(id, &comment);

    for m in &survivors {
        w.lib.land_mark(&m.id, "CERTIFIED", &m.tip, "");
        w.out(format!("queue.sh eject: {} returned to CERTIFIED", m.id));
    }
    if !pr.is_empty() && idents(w, "eject", &[("pr", &pr)]).is_ok() {
        w.forge.pr_close(&c.s.forge, &path, &pr);
    }
    let _ = fs::remove_file(&open);

    let surv = if survivors.is_empty() { "<none>".to_string() } else { crate::model::render_members(&survivors) };
    let mut body = format!("{id} ejected from PR {pr} by {actor}.");
    if !reason.is_empty() {
        body.push_str(&format!(" Reason: {reason}"));
    }
    body.push_str(&format!("\nSurvivors returned to CERTIFIED: {surv}"));
    w.lib.notify(&c.r.name, &format!("{id} ejected (queue.sh eject)"), &body);
    w.out(format!("queue.sh eject: ejected {id} from {} batch (landstate=RED)", c.r.name));
    OK
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
    let Ok(c) = resolve(w, "abandon", repo) else { return FAIL };
    let Ok(path) = repo_path(w, "abandon", &c) else { return FAIL };
    let lc_on = lifecycle_on(w);
    if lc_on && !dry_run && require_lc(w, "abandon").is_err() {
        return FAIL;
    }
    let Ok(_g) = take_lock(w, "abandon", &c, "") else { return FAIL };
    let open = c.queue_file("open");
    let kv = match records::read_kv(&open) {
        Ok(Some(kv)) => kv,
        _ => {
            w.err(format!("queue.sh abandon: no open batch for {}", c.r.name));
            return FAIL;
        }
    };
    if owner_refused(w, kv.get("owner").unwrap_or(""), &actor, "queue.sh abandon") {
        return FAIL;
    }
    let pr = kv.get_first("pr").unwrap_or("").to_string();
    let branch = kv.get_first("branch").unwrap_or("").to_string();
    let batch_id = kv.get_first("batch_id").unwrap_or("").to_string();
    let members = kv.members();
    let stamp = w.clock.stamp();
    let archive = c.queue_file(&format!("closed-pr{pr}-{stamp}"));
    let reason_clean = one_line(&reason);

    // RED and EJECTED are verdicts an abandon never overturns.
    let kept = |id: &str| -> Option<String> {
        records::land_state(&c.s.landstate, id).map(|s| s.state).filter(|s| s == "RED" || s == "EJECTED")
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
                    w.out(format!("dry-run: {}: return to CERTIFIED at {}", m.id, m.tip));
                    audit.push(format!("{}:CERTIFIED", m.id));
                }
            }
        }
        let audit = if audit.is_empty() { "<none>".to_string() } else { audit.join(",") };
        w.out(format!("dry-run: would cancel non-completed Gate run(s) for branch {branch}"));
        w.out(format!("dry-run: archive path: {}", archive.display()));
        w.out(format!(
            "dry-run: audit line (landing.log): QUEUE ABANDON {} repo={} pr={pr} actor={actor} members={audit} reason={reason_clean}",
            w.clock.now(),
            c.r.name
        ));
        return OK;
    }

    if lc_on && !batch_id.is_empty() {
        let r = bounded_text(&reason);
        match lc_cas(w, &batch_id, |s, v| w.lc.abandon_batch(&batch_id, s, v, "queue.sh", &r)) {
            Ok(()) => w.out(format!("queue.sh abandon: {batch_id} abandoned on spira-lc")),
            Err((rc, out)) => w.err(format!("queue.sh abandon: spira-lc abandon-batch refused for {batch_id} (rc={rc}): {out}")),
        }
    }
    if !branch.is_empty() && idents(w, "abandon", &[("branch", &branch)]).is_ok() {
        w.lib.cancel_runs(&c.s.forge, &path, &branch);
    }
    if !pr.is_empty() && idents(w, "abandon", &[("pr", &pr)]).is_ok() {
        w.forge.pr_comment(&c.s.forge, &path, &pr, &bounded_text(&format!("Batch abandoned. Reason: {reason}")));
        w.forge.pr_close(&c.s.forge, &path, &pr);
    }
    let mut audit = Vec::new();
    for m in &members {
        match kept(&m.id) {
            Some(st) => {
                w.out(format!("queue.sh abandon: {}: left as {st}", m.id));
                audit.push(format!("{}:{st}", m.id));
            }
            None => {
                w.lib.land_mark(&m.id, "CERTIFIED", &m.tip, "");
                w.out(format!("queue.sh abandon: {}: returned to CERTIFIED", m.id));
                audit.push(format!("{}:CERTIFIED", m.id));
            }
        }
    }
    let audit = if audit.is_empty() { "<none>".to_string() } else { audit.join(",") };

    // The archive keeps who and why, in the file itself.
    let mut archived = kv.clone();
    archived.push("reason", &reason_clean);
    archived.push("actor", &actor);
    if write_atomic(&open, &archived.render()).is_err() || fs::rename(&open, &archive).is_err() {
        w.err("queue.sh abandon: failed to archive open record");
        return FAIL;
    }
    landing_log(
        &c.s.run,
        &format!("QUEUE ABANDON {} repo={} pr={pr} actor={actor} members={audit} reason={reason_clean}", w.clock.now(), c.r.name),
    );
    w.lib.event(
        "queue.abandoned",
        &format!("abandoned PR {pr} for {} (actor={actor})", c.r.name),
        &format!("members={audit} reason={reason_clean}"),
    );
    w.lib.notify(&c.r.name, "batch abandoned (queue.sh abandon)", &format!("PR {pr} abandoned by {actor}. Reason: {reason_clean}\nMembers: {audit}"));
    w.out(format!("queue.sh abandon: PR {pr} closed, batch abandoned for {}", c.r.name));
    OK
}

/// queue_certified_list: every `spira/*` branch whose landstate is CERTIFIED, with the
/// landstate's tip and epoch.
fn certified(w: &World, c: &super::Ctx, path: &std::path::Path) -> Vec<(String, String, u64)> {
    let mut out = Vec::new();
    for (branch, _) in w.git.branches(path, "refs/heads/spira/") {
        let Some(id) = branch.strip_prefix("spira/") else { continue };
        if let Some(ls) = records::land_state(&c.s.landstate, id) {
            if ls.state == "CERTIFIED" {
                out.push((id.to_string(), ls.tip, ls.at));
            }
        }
    }
    out
}

pub fn open_batch(w: &World, repo: Option<&str>, members_arg: &Text, skip_pregate: bool, dry_run: bool) -> i32 {
    if !czar_ok(w) {
        return FAIL;
    }
    let members_arg = match read_text(w, members_arg) {
        Ok(m) => m,
        Err(e) => {
            w.err(format!("queue.sh open-batch: cannot read the members: {e}"));
            return USAGE;
        }
    };
    let actor = w.var("SPIRA_QUEUE_ACTOR").unwrap_or_else(|| "operator".into());
    let Ok(c) = resolve(w, "open-batch", repo) else { return FAIL };
    let Ok(path) = repo_path(w, "open-batch", &c) else { return FAIL };
    let name = c.r.name.clone();
    if c.r.mode != LandMode::Queue {
        w.err(format!("queue.sh open-batch: repo is not in queue mode (mode={})", c.r.mode.as_str()));
        return FAIL;
    }
    let lc_on = lifecycle_on(w);
    if lc_on && !dry_run && require_lc(w, "open-batch").is_err() {
        return FAIL;
    }
    let Ok(_g) = take_lock(w, "open-batch", &c, "") else { return FAIL };
    let open = c.queue_file("open");
    if open.exists() {
        let owner = records::read_kv(&open).ok().flatten().and_then(|k| k.get("owner").map(String::from)).unwrap_or_default();
        let owner = if owner.is_empty() { "batcher".into() } else { owner };
        w.err(format!("queue.sh open-batch: a batch is already open for {name} (owner={owner}) — no override; abandon it first (queue.sh abandon)"));
        return FAIL;
    }
    let Some(base) = c.r.landref.clone() else {
        w.err(format!("queue.sh open-batch: cannot resolve landing ref for {name}"));
        return FAIL;
    };
    let Some(base_sha) = w.git.rev_parse(&path, &base) else {
        w.err(format!("queue.sh open-batch: cannot resolve {base}"));
        return FAIL;
    };
    let remote = c.r.ref_remote(&base).unwrap_or_else(|| "origin".into());
    let base_branch = ref_branch(&base).to_string();

    let certs = certified(w, &c, &path);
    let mut skips: Vec<String> = Vec::new();
    let mut cands: Vec<(String, String)> = Vec::new();
    if !members_arg.trim().is_empty() {
        for mid in members_arg.split(|ch: char| ch == ',' || ch.is_whitespace()).filter(|s| !s.is_empty()) {
            match certs.iter().find(|(i, _, _)| i == mid) {
                Some((i, t, _)) => cands.push((i.clone(), t.clone())),
                None => skips.push(format!("{mid}: not CERTIFIED")),
            }
        }
    } else if !certs.is_empty() {
        let ids: Vec<String> = certs.iter().map(|(i, _, _)| i.clone()).collect();
        let prio = w.bd.show(&ids).ok().map(|rows| rows_json(&rows)).unwrap_or_else(|| "[]".into());
        let rows: String = certs.iter().fold(String::new(), |mut acc, (i, t, e)| {
            acc.push_str(&format!("{i} {t} {e}\n"));
            acc
        });
        cands = w.lib.sort_rows(&path, &base_sha, &prio, &rows);
    }

    // Admission: closed, or open and carrying the submitted label. An unreadable bead is
    // not admitted (DESIGN.md §8 D8).
    let ids: Vec<String> = cands.iter().map(|(i, _)| i.clone()).collect();
    let rows = if ids.is_empty() { Ok(Vec::new()) } else { w.bd.show(&ids) };
    let mut admitted = Vec::new();
    for (id, tip) in cands {
        let row = rows.as_ref().ok().and_then(|rs| rs.iter().find(|r| r.id == id));
        match row {
            None => skips.push(format!("{id}: bead status unknown (bd read failed) — not admitted")),
            Some(r) => {
                let st = r.status.clone().unwrap_or_default();
                if st != "closed" && !r.labels.iter().any(|l| l == &c.s.submitted_label) {
                    skips.push(format!("{id}: bead status={st} (not closed, not submitted)"));
                } else {
                    admitted.push((id, tip));
                }
            }
        }
    }

    let wt = c.s.run.join("worktree").join(format!(".open-batch-{name}-{}", w.env.pid()));
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
        if crate::ident::check("member", &id).is_err() || crate::ident::check("tip", &tip).is_err() {
            skips.push(format!("{id}: not a valid id:tip"));
            continue;
        }
        let subject = w.lib.land_subject(&id);
        if w.git.merge_no_ff(&wt, &subject, &tip) {
            members.push(Member { id, tip });
        } else {
            w.git.merge_abort(&wt);
            let why = if w.lib.base_conflict(&path, &base_sha, &tip) { "conflicts with base" } else { "conflicts with batch" };
            skips.push(format!("{id}: {why}"));
        }
    }
    for s in &skips {
        w.out(format!("queue.sh open-batch: skip — {s}"));
    }
    if members.is_empty() {
        w.out(format!("queue.sh open-batch: no cut — nothing admissible for {name}"));
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }
    w.lib.format_batch(&wt, &base_sha, &name);
    let head = w.git.rev_parse(&wt, "HEAD").unwrap_or_default();
    let stamp = w.clock.stamp();
    let batch_br = format!("spira/queue/{stamp}");
    let rendered = crate::model::render_members(&members);

    if dry_run {
        w.out(format!("queue.sh open-batch: dry-run for {name}"));
        w.out("members:");
        for m in &members {
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
        w.git.worktree_remove(&path, &wt);
        return OK;
    }

    w.git.branch_set(&path, &batch_br, &head, true);
    w.git.worktree_remove(&path, &wt);

    if skip_pregate {
        w.out("queue.sh open-batch: pre-flight gate skipped (--skip-pregate) — CI is the authority");
    } else {
        let (mut rc, out) = w.lib.pf_gate(&batch_br, &name, &stamp, c.s.preflight_wall_secs);
        if rc == 124 {
            w.out("queue.sh open-batch: pre-flight hit its wall — opening the PR, CI decides");
            rc = 0;
        }
        if rc != 0 {
            w.err(format!("queue.sh open-batch: local pre-flight gate failed for {batch_br} — not opening a batch"));
            w.err(out.trim_end_matches('\n'));
            w.err("queue.sh open-batch: retry with --skip-pregate to let CI be the gate");
            w.git.branch_delete(&path, &batch_br);
            return FAIL;
        }
    }
    if !w.lib.push(&path, &remote, &format!("{head}:refs/heads/{batch_br}")) {
        w.err(format!("queue.sh open-batch: could not push {batch_br}"));
        return FAIL;
    }
    let ids: Vec<String> = members.iter().map(|m| m.id.clone()).collect();
    let titles = w.bd.show(&ids).unwrap_or_default();
    let mut body = format!("Merge-queue batch: {} beads for {name}, onto {base_branch}.\n\n", members.len());
    if skip_pregate {
        body.push_str("Opened with --skip-pregate: the local pre-flight gate was not run for this batch; CI is the gate.\n\n");
    }
    for id in &ids {
        body.push_str(&format!("- {id} — {}\n", title_line(&titles, id)));
    }
    let title = format!("queue: {} beads for {name}", members.len());
    let Some(pr) = w.forge.pr_create(&c.s.forge, &path, &batch_br, &base_branch, &title, body.trim_end_matches('\n')) else {
        w.err(format!("queue.sh open-batch: forge pr-create failed for {batch_br}"));
        return FAIL;
    };
    if pr.is_empty() {
        w.err(format!("queue.sh open-batch: forge returned no PR number for {batch_br}"));
        return FAIL;
    }
    let opened = w.clock.now();
    let mut rec = Kv::default();
    for (k, v) in [
        ("pr", pr.as_str()),
        ("head", head.as_str()),
        ("base", base_sha.as_str()),
        ("members", rendered.as_str()),
        ("opened", &opened.to_string()),
        ("branch", batch_br.as_str()),
        ("owner", actor.as_str()),
    ] {
        rec.push(k, v);
    }
    if let Err(e) = write_atomic(&open, &rec.render()) {
        w.err(format!("queue.sh open-batch: {e}"));
        return FAIL;
    }

    let lc_id = format!("{name}-{stamp}");
    let csv = members.iter().map(Member::render).collect::<Vec<_>>().join(",");
    // Switch OFF: the pre-sp-o7nbr.5 record — no batch_id/version, no spira-lc call.
    let cut = if !lc_on {
        Ok(String::new())
    } else {
        for m in &members {
            w.lc.create_bead(&m.id);
        }
        w.lc.cut(&lc_id, &name, &head, &base_sha, &csv, "queue.sh")
    };
    match cut {
        Ok(version) if !version.is_empty() => {
            rec.push("batch_id", &lc_id);
            rec.push("version", &version);
            let _ = write_atomic(&open, &rec.render());
            w.out(format!("queue.sh open-batch: {lc_id} cut on spira-lc (version={version})"));
        }
        Ok(_) => {}
        Err((rc, out)) => w.err(format!("queue.sh open-batch: spira-lc cut refused for {lc_id} (rc={rc}): {out}")),
    }
    for m in &members {
        w.lib.land_mark(&m.id, "BATCHED", &m.tip, "");
    }
    let _ = fs::remove_file(c.s.run.join(format!("queue-stuck-{name}")));
    landing_log(&c.s.run, &format!("QUEUE BATCH {opened} repo={name} members={} gate_seconds=0 verdict=green source=open-batch", members.len()));
    w.out(format!("queue.sh open-batch: PR {pr} opened — {} branches ({batch_br})", members.len()));
    OK
}

/// The bd rows as the JSON array `queue_sort_rows` reads through PRIO_JSON.
fn rows_json(rows: &[crate::model::BeadRow]) -> String {
    let v: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| serde_json::json!({"id": r.id, "title": r.title, "status": r.status, "labels": r.labels, "priority": r.priority}))
        .collect();
    serde_json::Value::Array(v).to_string()
}
