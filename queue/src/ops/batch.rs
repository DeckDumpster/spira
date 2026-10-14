//! The queue.forge open batch: eject, abandon, open-batch (DESIGN.md §2.2).

use std::fs;

use super::{actor, czar_ok, lc_abandon, lc_eject_member, require_lc, idents, landing_log, owner_refused, read_text, repo_path, resolve, take_lock, title_line, World, FAIL, OK, USAGE};
use crate::cli::Text;
use crate::ident::bounded_text;
use crate::model::{EjectCause, LandMode, Member};
use crate::ports::ref_branch;
use crate::records::{self, one_line, write_atomic, Kv};

/// A hand eject's walk on spira-lc: Deliver first when the row is still CERTIFIED or SUBMITTED (a requeue of a member delivered from SUBMITTED lands there; Returned is
/// legal only from IN_DELIVERY), then Returned{batch-ejected} -> REWORK; a row already in REWORK is the target state, not a refusal. The refusal is
/// reported here and handed back: an eject that leaves the row where it was must exit non-zero.
pub fn lc_return(w: &World, id: &str, red: bool) -> Result<(), ()> {
    let fail = |why: String| {
        w.err(format!("queue.sh eject: spira-lc: {id}: {why} — not returned to REWORK on spira-lc"));
        Err(())
    };
    let Some((mut state, version)) = w.lc.bead_state(id) else { return fail("no lifecycle row".into()) };
    let Ok(mut v) = version.trim().parse::<u64>() else { return fail(format!("unreadable version {version:?}")) };
    if state == "REWORK" {
        return Ok(());
    }
    let who = "queue.sh";
    if state == "CERTIFIED" || state == "SUBMITTED" {
        if let Err((rc, e)) = w.lc.bead_event(id, &state, &v.to_string(), who, "\"Deliver\"") {
            return fail(format!("Deliver refused (rc={rc}): {e}"));
        }
        state = "IN_DELIVERY".into();
        v += 1;
    }
    if state != "IN_DELIVERY" {
        return fail(format!("in state {state}, not SUBMITTED, CERTIFIED or IN_DELIVERY"));
    }
    let reason = if red { "batch-ejected-red" } else { "batch-ejected" };
    if let Err((rc, e)) = w.lc.bead_event(id, &state, &v.to_string(), who, &format!("{{\"Returned\":{{\"reason\":\"{reason}\"}}}}")) {
        return fail(format!("Returned refused (rc={rc}): {e}"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn eject(w: &World, id: &str, repo: Option<&str>, reason: &Text, suites: &str, red: bool, harness_fault: bool, dry_run: bool) -> i32 {
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
    // --suites is a comma-separated list (queue.sh's contract, and what recertification
    // reads back): each name is an identifier, the commas are not.
    if !suites.is_empty() && suites.split(',').try_for_each(|s| idents(w, "eject", &[("suite", s)])).is_err() {
        return FAIL;
    }
    let actor = actor(w);
    let cause = EjectCause::decide(red, harness_fault, suites);
    if !dry_run && require_lc(w, "eject").is_err() {
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

    let Some(hit) = hit else {
        // Not batched: a CERTIFIED bead is withdrawn the way a reopen withdraws it.
        let st = w.lc.bead_row(id);
        if st.as_ref().map(|s| s.state.as_str()) != Some("CERTIFIED") {
            w.err(format!("queue.sh eject: {id} is not a member of the open batch for {} and is not CERTIFIED", c.r.name));
            let ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
            w.err(format!("batch members: {}", if ids.is_empty() { "<none>".to_string() } else { ids.join(" ") }));
            return FAIL;
        }
        if dry_run {
            let tip = st.and_then(|s| s.tip).unwrap_or_default();
            w.out(format!("dry-run: {id} is CERTIFIED but not yet batched for {} (tip={tip})", c.r.name));
            w.out(format!("dry-run: would reopen bead {id}, return it to REWORK on spira-lc and clear assignee"));
            w.out(format!("dry-run: would record cause {}", cause.as_str()));
            w.out(format!("dry-run: would post comment to {id}"));
            return bead_resolves(w, id);
        }
        let reopened = w.lib.bead_reopen(id, cause.as_str(), suites);
        if !reopened {
            w.err(format!("queue.sh eject: cannot reopen {id} (cause {})", cause.as_str()));
        }
        let returned = lc_return(w, id, cause == EjectCause::EjectRed).is_ok();
        let mut comment = format!("Ejected while certified but not yet batched in {}.", c.r.name);
        if !reason.is_empty() {
            comment.push_str(&format!("\n\n{reason}"));
        }
        comment.push_str("\n\nWithdrawn from the queue. Recertify the branch before it can rejoin.");
        if !suites.is_empty() {
            comment.push_str(&format!("\n\nRecertification will force these suites regardless of SPIRA_CERTIFY_SUITES: {suites}"));
        }
        w.lib.comment(id, &comment);
        if !reopened || !returned {
            return FAIL;
        }
        w.out(format!("queue.sh eject: ejected {id} (certified, not yet batched) for {} (withdrawn)", c.r.name));
        return OK;
    };

    if dry_run {
        w.out(format!("dry-run: {id} is in the open batch for {} (tip={})", c.r.name, hit.tip));
        w.out(format!("dry-run: would record cause {}", cause.as_str()));
        w.out(format!("dry-run: would return bead {id} to spira-lc via a Returned event"));
        w.out(format!("dry-run: would post comment to {id}"));
        w.out(format!("dry-run: would close PR {pr}"));
        if !survivors.is_empty() {
            let s: Vec<&str> = survivors.iter().map(|m| m.id.as_str()).collect();
            w.out(format!("dry-run: would return survivors to CERTIFIED: {}", s.join(" ")));
        }
        return bead_resolves(w, id);
    }

    let why = if reason.is_empty() { "ejected".to_string() } else { reason.clone() };
    // The cause row spira-claim classifies: eject (harness) or eject-red (judged). §8 D1.
    // `bead_reopen` carries the suites a recertification must force.
    w.lib.cause_event(id, cause.as_str());
    let mut failed = false;
    w.lib.release_claim(id);
    if !batch_id.is_empty() {
        let r = bounded_text(&why);
        match lc_eject_member(w, &batch_id, id, "queue.sh", &r) {
            Ok(()) => w.out(format!("queue.sh eject: {id} ejected on spira-lc")),
            Err((rc, out)) => {
                failed = true;
                w.err(format!("queue.sh eject: spira-lc eject-member refused for {id} (rc={rc}): {out}"));
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
        let r = bounded_text(&format!("{id} ejected; the batch PR is closed and not rebuilt"));
        match lc_abandon(w, &batch_id, "queue.sh", &r) {
            Ok(()) => w.out(format!("queue.sh eject: {batch_id} abandoned on spira-lc (survivors back to CERTIFIED)")),
            Err((rc, out)) => {
                failed = true;
                w.err(format!("queue.sh eject: spira-lc abandon-batch refused for {batch_id} (rc={rc}): {out}"));
            }
        }
    }
    let branch = kv.as_ref().and_then(|k| k.get_first("branch")).unwrap_or("").to_string();
    if !branch.is_empty() && idents(w, "eject", &[("branch", &branch)]).is_ok() {
        w.lib.cancel_runs(&c.s.forge, &path, &branch);
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
    w.lib.notify(&c.s.mailbox, &c.r.name, &format!("{id} ejected (queue.sh eject)"), &body);
    if failed {
        w.err(format!("queue.sh eject: {id} left the {} batch record but spira-lc did not take every write — see above", c.r.name));
        return FAIL;
    }
    w.out(format!("queue.sh eject: ejected {id} from {} batch {batch_id}; {} member(s) remain", c.r.name, survivors.len()));
    OK
}

/// A cut that answered "cannot tell" may have written the batch row: look before retrying, so a
/// deadline neither strands BATCHED members behind no PR nor cuts the same batch twice.
fn cut_settled(w: &World, id: &str, repo: &str, head: &str, base: &str, members: &str) -> Result<String, (i32, String)> {
    let mut last = Err((2, "no attempt".to_string()));
    for attempt in 0..3u32 {
        if attempt > 0 {
            w.clock.sleep(u64::from(attempt));
            if let Some((_, version)) = w.lc.batch_state(id) {
                return Ok(version);
            }
        }
        last = w.lc.cut(id, repo, head, base, members, "queue.sh");
        if !matches!(&last, Err((2, _))) {
            return last;
        }
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
    let Ok(c) = resolve(w, "abandon", repo) else { return FAIL };
    let Ok(path) = repo_path(w, "abandon", &c) else { return FAIL };
    if !dry_run && require_lc(w, "abandon").is_err() {
        return FAIL;
    }
    let Ok(_g) = take_lock(w, "abandon", &c, "") else { return FAIL };
    abandon_locked(w, &c, &path, &reason, &actor, dry_run, "abandon")
}

/// The abandon itself, with the repo's queue lock already held by the caller.
fn abandon_locked(w: &World, c: &super::Ctx, path: &std::path::Path, reason: &str, actor: &str, dry_run: bool, label: &str) -> i32 {
    let open = c.queue_file("open");
    let kv = match records::read_kv(&open) {
        Ok(Some(kv)) => kv,
        _ => {
            w.err(format!("queue.sh {label}: no open batch for {}", c.r.name));
            return FAIL;
        }
    };
    if owner_refused(w, kv.get("owner").unwrap_or(""), actor, &format!("queue.sh {label}")) {
        return FAIL;
    }
    let pr = kv.get_first("pr").unwrap_or("").to_string();
    let branch = kv.get_first("branch").unwrap_or("").to_string();
    let batch_id = kv.get_first("batch_id").unwrap_or("").to_string();
    if !pr.is_empty() && idents(w, label, &[("pr", &pr)]).is_ok() && w.forge.pr_state(&c.s.forge, path, &pr).as_deref().map(str::trim) == Some("merged") {
        w.err(format!("queue.sh {label}: refused — PR {pr} is already merged; its batch lands through verdict, not abandon"));
        return FAIL;
    }
    let members = kv.members();
    let stamp = w.clock.stamp();
    let archive = c.queue_file(&format!("closed-pr{pr}-{stamp}"));
    let reason_clean = one_line(reason);

    // A member already returned to REWORK is a verdict an abandon never overturns.
    let kept = |id: &str| -> Option<String> { w.lc.bead_row(id).map(|r| r.state).filter(|s| s == "REWORK") };

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

    if !batch_id.is_empty() {
        let r = bounded_text(reason);
        match lc_abandon(w, &batch_id, "queue.sh", &r) {
            Ok(()) => w.out(format!("queue.sh {label}: {batch_id} abandoned on spira-lc")),
            Err((rc, out)) => w.err(format!("queue.sh {label}: spira-lc abandon-batch refused for {batch_id} (rc={rc}): {out}")),
        }
    }
    if !branch.is_empty() && idents(w, label, &[("branch", &branch)]).is_ok() {
        w.lib.cancel_runs(&c.s.forge, path, &branch);
    }
    if !pr.is_empty() && idents(w, label, &[("pr", &pr)]).is_ok() {
        w.forge.pr_comment(&c.s.forge, path, &pr, &bounded_text(&format!("Batch abandoned. Reason: {reason}")));
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
    let audit = if audit.is_empty() { "<none>".to_string() } else { audit.join(",") };

    // The archive keeps who and why, in the file itself.
    let mut archived = kv.clone();
    archived.push("reason", &reason_clean);
    archived.push("actor", &actor);
    if write_atomic(&open, &archived.render()).is_err() || fs::rename(&open, &archive).is_err() {
        w.err(format!("queue.sh {label}: failed to archive open record"));
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
    w.lib.notify(&c.s.mailbox, &c.r.name, "batch abandoned (queue.sh abandon)", &format!("PR {pr} abandoned by {actor}. Reason: {reason_clean}\nMembers: {audit}"));
    w.out(format!("queue.sh {label}: PR {pr} closed, batch abandoned for {}", c.r.name));
    OK
}

/// The first prerequisite in `row`'s recorded stack, transitively, that is held, ejected at its
/// current tip, or whose row cannot be read; its dependent would carry that prerequisite's
/// commits into a batch.
fn barred_prereq(w: &World, row: &crate::model::LcBeadRow) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    let mut todo: Vec<String> = row.stack.clone();
    while let Some(p) = todo.pop() {
        if !seen.insert(p.clone()) {
            continue;
        }
        match w.lc.bead_row(&p) {
            None => return Some(p),
            Some(pr) if !pr.holds.is_empty() || (pr.ejected_red_tip.is_some() && pr.ejected_red_tip == pr.tip) => return Some(p),
            Some(pr) => todo.extend(pr.stack),
        }
    }
    None
}

pub fn open_batch(w: &World, repo: Option<&str>, members_arg: &Text, skip_pregate: bool, dry_run: bool, replace: Option<&Text>) -> i32 {
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
    let actor = w.var("SPIRA_QUEUE_ACTOR").unwrap_or_else(|| "operator".into());
    let Ok(c) = resolve(w, "open-batch", repo) else { return FAIL };
    let Ok(path) = repo_path(w, "open-batch", &c) else { return FAIL };
    let name = c.r.name.clone();
    if c.r.mode != LandMode::Queue {
        w.err(format!("queue.sh open-batch: repo is not in queue mode (mode={})", c.r.mode.as_str()));
        return FAIL;
    }
    if !dry_run && require_lc(w, "open-batch").is_err() {
        return FAIL;
    }
    let Ok(_g) = take_lock(w, "open-batch", &c, "") else { return FAIL };
    let open = c.queue_file("open");
    if open.exists() && replace_reason.is_some() && !dry_run {
        let r = replace_reason.as_deref().unwrap_or_default();
        if abandon_locked(w, &c, &path, r, &actor, false, "open-batch --replace") != OK {
            return FAIL;
        }
    }
    if open.exists() && !(dry_run && replace_reason.is_some()) {
        let owner = records::read_kv(&open).ok().flatten().and_then(|k| k.get("owner").map(String::from)).unwrap_or_default();
        let owner = if owner.is_empty() { "batcher".into() } else { owner };
        w.err(format!("queue.sh open-batch: a batch is already open for {name} (owner={owner}) — abandon it first (queue.sh abandon) or pass --replace --reason"));
        return FAIL;
    }
    if !(dry_run && replace_reason.is_some()) {
        if let Some((pr, br)) = w.forge.queue_prs(&c.s.forge, &path).and_then(|v| v.into_iter().next()) {
            w.err(format!("queue.sh open-batch: {name} already has open batch PR {pr} ({br}) on the forge — nothing cut; abandon it first (queue.sh abandon) or pass --replace --reason"));
            return FAIL;
        }
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

    let branches = w.git.branches(&path, "refs/heads/spira/");
    let certs = super::helpers::certified_rows(w.lc, &branches);
    let submitted = super::helpers::state_rows(w.lc, &branches, "SUBMITTED");
    let mut skips: Vec<String> = Vec::new();
    let mut cands: Vec<(String, String)> = Vec::new();
    if !members_arg.trim().is_empty() {
        for mid in members_arg.split(|ch: char| ch == ',' || ch.is_whitespace()).filter(|s| !s.is_empty()) {
            match certs.iter().chain(submitted.iter()).find(|(i, _, _)| i == mid) {
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
            let prio = w.bd.show(&ids).ok().map(|rows| rows_json(&rows)).unwrap_or_else(|| "[]".into());
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
            None => skips.push(format!("{id}: no lifecycle row (spira-lc could not say) — not admitted")),
            Some(r) if matches!(r.state.as_str(), "CERTIFIED" | "SUBMITTED") && super::round::batch_red_at(&c, &id, &tip) => {
                skips.push(format!("{id}: {tip} already went red in a batch — not admitted until the tip changes"))
            }
            Some(r) if matches!(r.state.as_str(), "CERTIFIED" | "SUBMITTED") && !r.holds.is_empty() => {
                skips.push(format!("{id}: held ({}) — not admitted", r.holds.join(",")))
            }
            Some(r) if matches!(r.state.as_str(), "CERTIFIED" | "SUBMITTED") => match barred_prereq(w, &r) {
                Some(p) => skips.push(format!("{id}: stacked on held {p} — not admitted")),
                None => {
                    if r.state == "SUBMITTED" {
                        if let Some(why) = super::round::gate_red_at(&c, &id, &tip) {
                            skips.push(format!("{id}: its gate is FAIL at {tip} ({why}) — not admitted"));
                            continue;
                        }
                        provisional.push(id.clone());
                    }
                    admitted.push((id, tip))
                }
            },
            Some(r) => skips.push(format!("{id}: lifecycle state={} (no longer SUBMITTED or CERTIFIED) — not admitted", r.state)),
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
        if super::round::merge_member(w, &c, &wt, &id, &tip) {
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
    let lc_id = format!("{name}-{stamp}");
    let csv = members.iter().map(Member::render).collect::<Vec<_>>().join(",");
    for m in &members {
        w.lc.create_bead(&m.id);
    }
    let version = match cut_settled(w, &lc_id, &name, &head, &base_sha, &csv) {
        Ok(v) => v,
        Err((rc, out)) => {
            w.err(format!("queue.sh open-batch: spira-lc cut refused for {lc_id} (rc={rc}): {out} — no PR opened"));
            w.git.branch_delete(&path, &batch_br);
            return FAIL;
        }
    };
    let undo = |why: &str| {
        let _ = lc_abandon(w, &lc_id, "queue.sh", why);
        w.git.branch_delete(&path, &batch_br);
    };
    if !w.lib.push(&path, &remote, &format!("{head}:refs/heads/{batch_br}")) {
        w.err(format!("queue.sh open-batch: could not push {batch_br}"));
        undo("open-batch could not push the batch branch");
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
        undo("open-batch could not open the PR");
        return FAIL;
    };
    if pr.is_empty() {
        w.err(format!("queue.sh open-batch: forge returned no PR number for {batch_br}"));
        undo("open-batch got no PR number");
        return FAIL;
    }
    let armed = w.forge.pr_automerge(&c.s.forge, &path, &pr);
    if !armed {
        w.err(format!("queue.sh open-batch: PR {pr} for {name} is open but auto-merge could not be armed — the reconciler arms it on the next pass"));
    }
    let opened = w.clock.now();
    let provisional = provisional.iter().filter(|p| members.iter().any(|m| &m.id == *p)).cloned().collect::<Vec<_>>().join(" ");
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
    if !provisional.is_empty() {
        rec.push("provisional", &provisional);
        w.out(format!("queue.sh open-batch: provisional (SUBMITTED, gate still running): {provisional}"));
    }
    if let Err(e) = write_atomic(&open, &rec.render()) {
        w.err(format!("queue.sh open-batch: {e}"));
        undo("open-batch could not write the open record");
        return FAIL;
    }
    if !version.is_empty() {
        rec.push("batch_id", &lc_id);
        rec.push("version", &version);
        let _ = write_atomic(&open, &rec.render());
        w.out(format!("queue.sh open-batch: {lc_id} cut on spira-lc (version={version})"));
    }
    let _ = fs::remove_file(c.s.run.join(format!("queue-stuck-{name}")));
    landing_log(&c.s.run, &format!("QUEUE BATCH {opened} repo={name} members={} gate_seconds=0 verdict=green source=open-batch", members.len()));
    w.out(format!("queue.sh open-batch: PR {pr} opened — {} branches ({batch_br})", members.len()));
    if armed { OK } else { FAIL }
}

/// The bd rows as the JSON array `queue_sort_rows` reads through PRIO_JSON.
fn rows_json(rows: &[crate::model::BeadRow]) -> String {
    let v: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| serde_json::json!({"id": r.id, "title": r.title, "status": r.status, "labels": r.labels, "priority": r.priority}))
        .collect();
    serde_json::Value::Array(v).to_string()
}
