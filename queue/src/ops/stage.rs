//! `queue round stage | stage-test | promote | discard`: the next round assembled and fenced
//! behind the one whose suites are still running, tested the moment that one is green, and cut
//! when it lands (STAGED -> OPEN, or STAGED -> DISCARDED). A staged round moves no bead: only
//! `promote` delivers its members, so discarding one returns nothing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::round::{
    assemble, certify, csv, finish, handle_of, live_records, local_ctx, lock, mark_running,
    phase_of, phase_reds, record_of, rounds_dir, set, suite_statuses, tail, unjudgeable, Ride,
    BLOCKING, BUILD_RED, LC_ACTOR, UNIT_RED,
};
use super::{
    actor, idents, lc_cas, read_text, require_lc, round_event, Ctx, World, FAIL, OK, USAGE,
};
use crate::cli::Text;
use crate::ident::bounded_text;
use crate::model::{parse_members, render_members, Member};
use crate::records::{self, one_line, write_atomic, Kv};

pub const STAGED: &str = "round-staged";

/// The staged round's own testing phase, which the lifecycle does not model (it holds STAGED).
fn set_staged_phase(w: &World, kv: &mut Kv, phase: &str) {
    set(kv, "phase", phase);
    set(kv, "phase_at", &w.clock.now().to_string());
}

fn staged_record(c: &Ctx) -> Option<Kv> {
    records::read_kv(&c.queue_file(STAGED)).ok().flatten()
}

fn branch_of(batch: &str) -> String {
    format!("spira/round-staged/{batch}")
}

/// Refuses the members past the most one cut can write within the store's limits, to a later
/// round.
pub(super) fn refuse_over_cap<T>(
    admitted: &mut Vec<T>,
    skips: &mut Vec<String>,
    id: impl Fn(&T) -> &str,
) {
    let cap = state_store::limits::MAX_CUT_MEMBERS;
    for m in admitted.split_off(cap.min(admitted.len())) {
        skips.push(format!(
            "{}: the round is full at {cap} members — not admitted",
            id(&m)
        ));
    }
}

type Admitted = (Vec<Member>, BTreeMap<String, Vec<String>>, Vec<String>, Vec<String>);

/// The members a stage or promote may take: the admitted ones blocker-first, each one's unlanded
/// blockers, the skips, and the admitted members still SUBMITTED (provisional).
fn push_provisional(kv: &mut Kv, members: &[Member], provisional: &[String]) {
    let ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).filter(|id| provisional.iter().any(|p| p == id)).collect();
    if !ids.is_empty() {
        kv.push("provisional", &ids.join(" "));
    }
}

fn admit(w: &World, by: &str, wanted: Vec<Member>) -> Admitted {
    let mut provisional = Vec::new();
    let mut skips = Vec::new();
    let mut admitted = Vec::new();
    let mut blocked_by = BTreeMap::new();
    for m in wanted {
        match w.lc.bead_row(&m.id) {
            None => skips.push(format!(
                "{}: no lifecycle row (spira-lc could not say) — not admitted",
                m.id
            )),
            Some(r) if r.state != "CERTIFIED" && r.state != "SUBMITTED" => skips.push(format!(
                "{}: lifecycle state={} (not SUBMITTED or CERTIFIED) — not admitted",
                m.id, r.state
            )),
            Some(r) if r.state == "SUBMITTED" && super::round::submitted_hold(&r, &m.id, by).is_some() => {
                skips.push(super::round::submitted_hold(&r, &m.id, by).unwrap_or_default())
            }
            Some(r) => match r.tip.filter(|t| !t.is_empty()) {
                Some(tip) if tip.starts_with(&m.tip) => {
                    if r.state == "SUBMITTED" {
                        provisional.push(m.id.clone());
                    }
                    blocked_by.insert(m.id.clone(), r.blocked_by);
                    admitted.push(Member { id: m.id, tip })
                }
                Some(tip) => skips.push(format!(
                    "{}: {} is not its row's tip {tip} — not admitted",
                    m.id, m.tip
                )),
                None => skips.push(format!("{}: no submitted tip — not admitted", m.id)),
            },
        }
    }
    refuse_over_cap(&mut admitted, &mut skips, |m| &m.id);
    blocked_by.retain(|id, _| admitted.iter().any(|m| &m.id == id));
    provisional.retain(|id| admitted.iter().any(|m| &m.id == id));
    (super::round::blocker_first(admitted, &blocked_by), blocked_by, skips, provisional)
}

pub fn stage(
    w: &World,
    by: &str,
    repo: Option<&str>,
    members_arg: &Text,
    name: Option<&str>,
    worktree: Option<&Path>,
) -> i32 {
    let label = "round stage";
    let text = match read_text(w, members_arg) {
        Ok(t) => t,
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot read the members: {e}"));
            return USAGE;
        }
    };
    let wanted = parse_members(&text);
    if wanted.is_empty() {
        w.err(format!("queue.sh {label}: --members names no bead"));
        return USAGE;
    }
    let Ok((c, path)) = local_ctx(w, label, repo) else {
        return FAIL;
    };
    let repo_name = c.r.name.clone();
    if let Some(n) = name {
        if idents(w, label, &[("name", n)]).is_err() {
            return FAIL;
        }
    }
    if let Some(wt) = worktree {
        if let Err(e) = crate::ident::check_path("worktree", &wt.display().to_string()) {
            w.err(format!("queue.sh {label}: {e} — refused"));
            return FAIL;
        }
    }
    for m in &wanted {
        if idents(w, label, &[("member id", &m.id)]).is_err()
            || (!m.tip.is_empty() && idents(w, label, &[("member tip", &m.tip)]).is_err())
        {
            return FAIL;
        }
    }
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let g = match lock(w, label, &c) {
        Ok(g) => g,
        // A busy lock keeps its own exit (LOCK_BUSY), so a caller retries on the code.
        Err(rc) => return rc,
    };
    let _stage_lock = match crate::lock::try_stage_lock(&c.s.queue_dir, &repo_name) {
        crate::lock::Acquire::Held(g) => g,
        crate::lock::Acquire::Busy => {
            w.err(format!(
                "queue.sh {label}: another stage for {repo_name} is running its fences — retry"
            ));
            return super::LOCK_BUSY;
        }
        crate::lock::Acquire::Unopenable => {
            w.err(format!(
                "queue.sh {label}: cannot open the stage lock for {repo_name}"
            ));
            return FAIL;
        }
    };
    let open = match live_records(w, &c) {
        Ok(open) => open,
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot tell which rounds are open for {repo_name}: {e} — refused"));
            return FAIL;
        }
    };
    let Some(running) = open.into_iter().next() else {
        w.err(format!("queue.sh {label}: no round is open for {repo_name} — there is nothing to stage behind; use round open"));
        return FAIL;
    };
    let parent = running.get("batch_id").unwrap_or("").to_string();
    let parent_head = running.get("head").unwrap_or("").to_string();
    let ahead = running.members();
    if let Some(kv) = staged_record(&c) {
        w.err(format!("queue.sh {label}: a staged round is already waiting for {repo_name} ({}) — promote or discard it first", kv.get("batch_id").unwrap_or("?")));
        return FAIL;
    }

    let mut skips: Vec<String> = Vec::new();
    let (wanted, in_ahead): (Vec<Member>, Vec<Member>) = wanted
        .into_iter()
        .partition(|m| !ahead.iter().any(|a| a.id == m.id));
    for m in in_ahead {
        skips.push(format!(
            "{}: already a member of {parent} — not staged",
            m.id
        ));
    }
    let Some(landed_sha) = c.r.landref.as_deref().and_then(|base| w.git.rev_parse(&path, base)) else {
        w.err(format!(
            "queue.sh {label}: cannot resolve the landing ref for {repo_name}"
        ));
        return FAIL;
    };
    let (admitted, blocked_by, more, provisional) = admit(w, by, wanted);
    skips.extend(more);

    let batch = name
        .map(str::to_string)
        .unwrap_or_else(|| format!("{repo_name}-{}", w.clock.stamp()));
    let wt = worktree.map(Path::to_path_buf).unwrap_or_else(|| {
        c.s.run
            .join("worktree")
            .join(format!(".round-staged-{repo_name}"))
    });
    w.git.worktree_prune(&path);
    w.git.worktree_remove(&path, &wt);
    if let Some(p) = wt.parent() {
        let _ = fs::create_dir_all(p);
    }
    if !w.git.worktree_add_detached(&path, &wt, &parent_head) {
        w.err(format!(
            "queue.sh {label}: cannot create the staged worktree {}",
            wt.display()
        ));
        return FAIL;
    }
    let in_force = super::in_force::InForce::new(w, &c, &path, &landed_sha);
    // A record's tip may be a rebase onto its round, which would carry the others' paths.
    let mut riding: Vec<(String, String)> = ahead
        .iter()
        .map(|a| (a.id.clone(), w.lc.bead_row(&a.id).and_then(|r| r.tip).unwrap_or_else(|| a.tip.clone())))
        .collect();
    let mut merged: Vec<Member> = Vec::new();
    for m in admitted {
        let blocker = blocked_by.get(&m.id).and_then(|bs| {
            bs.iter()
                .find(|b| !merged.iter().chain(ahead.iter()).any(|x| &x.id == *b))
        });
        if let Some(b) = blocker {
            skips.push(format!("{}: blocked by {b}, which has not landed and is not merged ahead of it — not staged", m.id));
            continue;
        }
        if let Some((_, why)) = super::in_force::refusal(&m.id, in_force.not_in_force(&m.id, &riding), "staged") {
            skips.push(why);
            continue;
        }
        if super::round::merge_member(w, &c, &wt, &m.id, &m.tip) {
            riding.push((m.id.clone(), m.tip.clone()));
            merged.push(m);
        } else {
            w.git.merge_abort(&wt);
            skips.push(format!(
                "{}: conflicts with the round it would follow",
                m.id
            ));
        }
    }
    for s in &skips {
        w.out(format!("queue.sh {label}: skip — {s}"));
    }
    if merged.is_empty() {
        w.err(format!(
            "queue.sh {label}: nothing stageable for {repo_name}"
        ));
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }
    let head = w.git.rev_parse(&wt, "HEAD").unwrap_or_default();

    let branch = branch_of(&batch);
    if !w.git.branch_set(&path, &branch, &head, true) {
        w.err(format!(
            "queue.sh {label}: cannot name the staged head {head} as {branch} for the fences"
        ));
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }
    drop(g);
    let Some(lock_wait) = super::simple::gate_lock_wait(w, label, &c) else {
        w.git.worktree_remove(&path, &wt);
        w.git.branch_delete_sanctioned(&path, &branch);
        return FAIL;
    };
    let (rc, out) = w.scripts.gate(&branch, &repo_name, "", &lock_wait);
    let _g = match lock(w, label, &c) {
        Ok(g) => g,
        Err(rc) => {
            w.git.worktree_remove(&path, &wt);
            w.git.branch_delete_sanctioned(&path, &branch);
            return rc;
        }
    };
    let moved = match live_records(w, &c) {
        Ok(open) => {
            open.first()
                .and_then(|r| r.get("batch_id").map(str::to_string))
                .as_deref()
                != Some(parent.as_str())
                || staged_record(&c).is_some()
        }
        Err(_) => true,
    };
    if moved {
        w.err(format!("queue.sh {label}: the round {parent} ended or another stage took its place while the fences ran — not staged; retry"));
        w.git.worktree_remove(&path, &wt);
        w.git.branch_delete_sanctioned(&path, &branch);
        return FAIL;
    }
    if rc != 0 {
        let infra = super::simple::gate_outcome(rc) == "NO_VERDICT";
        let what = if infra {
            "the fences could not run (infra, not red) on"
        } else {
            "the fences are red on"
        };
        w.err(format!(
            "queue.sh {label}: {what} the staged head {head} — not staged{}\n{}",
            if infra { "; retry" } else { "" },
            tail(&out, 20)
        ));
        w.git.worktree_remove(&path, &wt);
        w.git.branch_delete_sanctioned(&path, &branch);
        return if infra { rc } else { FAIL };
    }

    if let Err((rc, out)) = w.lc.stage(
        &batch,
        &repo_name,
        &head,
        &parent_head,
        &csv(&merged),
        &parent,
        LC_ACTOR,
    ) {
        w.err(format!(
            "queue.sh {label}: spira-lc stage refused for {batch} (rc={rc}): {out}"
        ));
        w.git.worktree_remove(&path, &wt);
        w.git.branch_delete_sanctioned(&path, &branch);
        return FAIL;
    }
    let now = w.clock.now().to_string();
    let mut kv = Kv::default();
    kv.push("batch_id", &batch);
    kv.push("repo", &repo_name);
    kv.push("parent", &parent);
    kv.push("head", &head);
    kv.push("base", &parent_head);
    kv.push("members", &render_members(&merged));
    push_provisional(&mut kv, &merged, &provisional);
    kv.push("worktree", &wt.display().to_string());
    kv.push("actor", &actor(w));
    kv.push("opened", &now);
    set_staged_phase(w, &mut kv, "staged");
    if let Err(e) = write_atomic(&c.queue_file(STAGED), &kv.render()) {
        w.err(format!(
            "queue.sh {label}: cannot write the staged record: {e}"
        ));
        return FAIL;
    }
    round_event(
        w,
        &c.s.run,
        "stage",
        &repo_name,
        &batch,
        by,
        &[
            ("behind", parent.to_string()),
            ("head", head.to_string()),
            ("members", csv(&merged)),
        ],
    );
    w.out(format!("batch={batch}"));
    w.out(format!("behind={parent}"));
    w.out(format!("head={head}"));
    w.out(format!("members={}", render_members(&merged)));
    OK
}

fn results_dir(c: &Ctx, batch: &str) -> PathBuf {
    rounds_dir(c).join(format!("{batch}.results"))
}

pub fn stage_test(w: &World, by: &str, repo: Option<&str>) -> i32 {
    let label = "round stage-test";
    let Ok((c, path)) = local_ctx(w, label, repo) else {
        return FAIL;
    };
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let (batch, head, wt) = {
        let _g = match lock(w, label, &c) {
            Ok(g) => g,
            // A busy lock keeps its own exit (LOCK_BUSY), so a caller retries on the code.
            Err(rc) => return rc,
        };
        let Some(mut kv) = staged_record(&c) else {
            w.err(format!(
                "queue.sh {label}: no staged round for {}",
                c.r.name
            ));
            return FAIL;
        };
        let parent = kv.get("parent").unwrap_or("").to_string();
        match record_of(w, &c, &parent) {
            Err(e) => {
                w.err(format!(
                    "queue.sh {label}: cannot tell whether {parent} is still open: {e} — refused"
                ));
                return FAIL;
            }
            Ok(Some(_)) if phase_of(w, &parent) == "green" => {}
            Ok(Some(_)) => {
                w.err(format!("queue.sh {label}: {parent} is {}, not green — the staged round is tested once it is", phase_of(w, &parent)));
                return FAIL;
            }
            Ok(None) => {
                w.err(format!("queue.sh {label}: {parent} is no longer the open round — promote or discard the staged round"));
                return FAIL;
            }
        }
        let head = kv.get("head").unwrap_or("").to_string();
        let wt = PathBuf::from(kv.get("worktree").unwrap_or(""));
        if w.git.rev_parse(&wt, "HEAD").as_deref() != Some(head.as_str()) {
            w.err(format!("queue.sh {label}: the staged worktree {} is not at the staged head {head} — refused", wt.display()));
            return FAIL;
        }
        set_staged_phase(w, &mut kv, "testing");
        kv.remove("red");
        kv.remove("tested_tree");
        if let Err(e) = write_atomic(&c.queue_file(STAGED), &kv.render()) {
            w.err(format!(
                "queue.sh {label}: cannot write the staged record: {e}"
            ));
            return FAIL;
        }
        (kv.get("batch_id").unwrap_or("").to_string(), head, wt)
    };

    let Some(base) = c.r.landref.clone() else {
        w.err(format!(
            "queue.sh {label}: cannot resolve the landing ref of {}",
            c.r.name
        ));
        return FAIL;
    };
    let results = results_dir(&c, &batch);
    let _ = fs::remove_dir_all(&results);
    let _ = fs::create_dir_all(rounds_dir(&c));
    let out = w.scripts.round_vm(
        &wt,
        &results,
        &base,
        (&batch, &c.r.name),
        c.s.round_wall_secs,
        &handle_of(&c, &batch),
        None,
        None,
        None,
        false,
        None,
    );
    let _ = fs::remove_file(handle_of(&c, &batch));
    let mut found = Vec::new();
    suite_statuses(&results, &mut found, 0);
    let mut reds: Vec<String> = Vec::new();
    // Each test phase's red is a record; a unit red ends the pass before any suite (sp-cddf76).
    let phase_red = phase_reds(&results);
    let ended_by_unit = out.rc == 1 && phase_red.iter().any(|p| p.before_suites);
    for p in &phase_red {
        reds.push(UNIT_RED.into());
        for line in p.report() {
            w.out(line);
        }
    }
    let fault = match out.rc {
        1 if ended_by_unit => None,
        0 | 1 if found.is_empty() => Some("round-vm ran and left no verdicts — a fault of the round machinery, not of the candidates".to_string()),
        0 | 1 => unjudgeable(&wt, &found),
        4 => {
            reds.push(BUILD_RED.into());
            None
        }
        124 | 137 => Some(format!("round-vm exceeded the {}s wall", c.s.round_wall_secs)),
        rc => Some(format!("round-vm exited {rc}")),
    };
    reds.extend(
        found
            .iter()
            .filter(|(_, s)| BLOCKING.contains(&s.as_str()))
            .map(|(n, _)| n.clone()),
    );
    reds.sort();
    reds.dedup();

    let _g = match lock(w, label, &c) {
        Ok(g) => g,
        // A busy lock keeps its own exit (LOCK_BUSY), so a caller retries on the code.
        Err(rc) => return rc,
    };
    let Some(mut kv) = staged_record(&c) else {
        w.err(format!("queue.sh {label}: the staged round {batch} was promoted or discarded while it was tested — the result is dropped"));
        return FAIL;
    };
    if kv.get("batch_id") != Some(batch.as_str())
        || kv.get("phase") != Some("testing")
        || kv.get("head") != Some(head.as_str())
    {
        w.err(format!("queue.sh {label}: the staged round {batch} changed while it was tested — the result is dropped"));
        return FAIL;
    }
    if let Some(why) = fault {
        set_staged_phase(w, &mut kv, "fault");
        let _ = write_atomic(&c.queue_file(STAGED), &kv.render());
        w.err(format!(
            "queue.sh {label}: {why}; the staged round is not judged\n{}",
            tail(&out.err, 20)
        ));
        return super::round::FAULT;
    }
    if !reds.is_empty() {
        set(&mut kv, "red", &reds.join(","));
        set_staged_phase(w, &mut kv, "red");
        let _ = write_atomic(&c.queue_file(STAGED), &kv.render());
        w.out(format!(
            "queue.sh {label}: staged round {batch} is RED at {head}: {}",
            reds.join(",")
        ));
        w.out(format!("red={}", reds.join(",")));
        return FAIL;
    }
    let Some(tree) = w.git.rev_parse(&path, &format!("{head}^{{tree}}")) else {
        w.err(format!(
            "queue.sh {label}: cannot resolve {head}^{{tree}} — nothing can say what was judged"
        ));
        return FAIL;
    };
    set(&mut kv, "tested_tree", &tree);
    set_staged_phase(w, &mut kv, "green");
    if let Err(e) = write_atomic(&c.queue_file(STAGED), &kv.render()) {
        w.err(format!(
            "queue.sh {label}: cannot write the staged record: {e}"
        ));
        return FAIL;
    }
    round_event(
        w,
        &c.s.run,
        "stage-green",
        &c.r.name,
        &batch,
        by,
        &[("head", head.to_string()), ("tree", tree.to_string())],
    );
    w.out(format!(
        "queue.sh {label}: staged round {batch} is GREEN at {head} (tree {tree})"
    ));
    OK
}

/// Drop the staged round: its spira-lc row, record, worktree and fence branch. The caller holds
/// the lock. A row spira-lc no longer holds STAGED (already terminal, or never written) is gone
/// already; one that has been promoted is not ours to discard.
pub(super) fn discard_staged(
    w: &World,
    by: &str,
    c: &Ctx,
    path: &Path,
    kv: &Kv,
    reason: &str,
) -> Result<(), String> {
    let batch = kv.get("batch_id").unwrap_or("").to_string();
    let why = bounded_text(reason);
    let event = serde_json::json!({"Discard": {"reason": why}}).to_string();
    match w.lc.batch_state(&batch) {
        Some((state, _)) if state == "STAGED" => {
            lc_cas(w, &batch, |s, v| {
                w.lc.batch_event(&batch, s, v, LC_ACTOR, &event, None)
            })
            .map_err(|(rc, out)| format!("spira-lc refused to discard {batch} (rc={rc}): {out}"))?;
        }
        Some((state, _))
            if !matches!(
                state.as_str(),
                "LANDED" | "SETTLED" | "ABANDONED" | "DISCARDED"
            ) =>
        {
            return Err(format!(
                "spira-lc has {batch} {state}, not STAGED — it is not discarded"
            ));
        }
        _ => {}
    }
    let _ = fs::remove_file(c.queue_file(STAGED));
    w.git
        .worktree_remove(path, &PathBuf::from(kv.get("worktree").unwrap_or("")));
    w.git.branch_delete_sanctioned(path, &branch_of(&batch));
    crate::lock::remove_certify_lock(&c.s.queue_dir, &c.r.name, &batch);
    finish(
        w,
        c,
        &batch,
        super::round::End::Abandoned("discarded"),
        reason,
    );
    round_event(
        w,
        &c.s.run,
        "discard",
        &c.r.name,
        &batch,
        by,
        &[("reason", one_line(reason))],
    );
    Ok(())
}

/// The round `parent` can no longer be followed: discard the round staged behind it, if any.
pub(super) fn discard_behind(
    w: &World,
    by: &str,
    c: &Ctx,
    path: &Path,
    parent: &str,
    reason: &str,
) {
    let Some(kv) = staged_record(c) else { return };
    if kv.get("parent") != Some(parent) {
        return;
    }
    match discard_staged(w, by, c, path, &kv, reason) {
        Ok(()) => w.out(format!(
            "queue.sh: staged round {} discarded — {reason}",
            kv.get("batch_id").unwrap_or("?")
        )),
        Err(e) => w.err(format!(
            "queue.sh: the staged round {} behind {parent} was not discarded: {e}",
            kv.get("batch_id").unwrap_or("?")
        )),
    }
}

pub fn discard(w: &World, by: &str, repo: Option<&str>, reason: &Text) -> i32 {
    let label = "round discard";
    let reason = read_text(w, reason).unwrap_or_default();
    if reason.trim().is_empty() {
        w.err(format!("queue.sh {label}: --reason is required — pass --reason \"<why>\" so the discard is auditable"));
        return USAGE;
    }
    let Ok((c, path)) = local_ctx(w, label, repo) else {
        return FAIL;
    };
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let _g = match lock(w, label, &c) {
        Ok(g) => g,
        // A busy lock keeps its own exit (LOCK_BUSY), so a caller retries on the code.
        Err(rc) => return rc,
    };
    let Some(kv) = staged_record(&c) else {
        w.err(format!(
            "queue.sh {label}: no staged round for {}",
            c.r.name
        ));
        return FAIL;
    };
    match discard_staged(w, by, &c, &path, &kv, &reason) {
        Ok(()) => {
            w.out(format!(
                "queue.sh {label}: staged round {} discarded",
                kv.get("batch_id").unwrap_or("?")
            ));
            OK
        }
        Err(e) => {
            w.err(format!("queue.sh {label}: {e}"));
            FAIL
        }
    }
}

pub fn promote(w: &World, by: &str, repo: Option<&str>) -> i32 {
    let label = "round promote";
    let Ok((c, path)) = local_ctx(w, label, repo) else {
        return FAIL;
    };
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let repo_name = c.r.name.clone();
    let (batch, head, members, attested_from) = {
        let _g = match lock(w, label, &c) {
            Ok(g) => g,
            // A busy lock keeps its own exit (LOCK_BUSY), so a caller retries on the code.
            Err(rc) => return rc,
        };
        let Some(kv) = staged_record(&c) else {
            w.err(format!("queue.sh {label}: no staged round for {repo_name}"));
            return FAIL;
        };
        let batch = kv.get("batch_id").unwrap_or("").to_string();
        let parent = kv.get("parent").unwrap_or("").to_string();
        let running = match record_of(w, &c, &parent) {
            Ok(r) => r,
            Err(e) => {
                w.err(format!(
                    "queue.sh {label}: cannot tell whether {parent} is still open: {e} — refused"
                ));
                return FAIL;
            }
        };
        if let Some(running) = running {
            w.err(format!(
                "queue.sh {label}: round {} is still open for {repo_name} (phase {}) — {batch} follows {parent} and is promoted once it has landed",
                running.get("batch_id").unwrap_or("?"),
                phase_of(w, running.get("batch_id").unwrap_or(""))
            ));
            return FAIL;
        }
        let parent_state =
            w.lc.batch_state(&parent)
                .map(|(s, _)| s)
                .unwrap_or_default();
        if parent_state != "LANDED" {
            let why = format!(
                "the round it followed, {parent}, is {}, not LANDED",
                if parent_state.is_empty() {
                    "unknown"
                } else {
                    parent_state.as_str()
                }
            );
            return discarded(w, by, &c, &path, &kv, &why);
        }
        let Some(base) = c.r.landref.clone() else {
            w.err(format!(
                "queue.sh {label}: cannot resolve landing ref for {repo_name}"
            ));
            return FAIL;
        };
        let Some(base_sha) = w.git.rev_parse(&path, &base) else {
            w.err(format!("queue.sh {label}: cannot resolve {base}"));
            return FAIL;
        };

        let staged_members = kv.members();
        let (admitted, blocked_by, skips, provisional) = admit(w, by, staged_members.clone());
        let tips_kept = admitted.len() == staged_members.len()
            && admitted
                .iter()
                .all(|a| staged_members.iter().any(|s| s.id == a.id && s.tip == a.tip));
        if !tips_kept {
            return discarded(
                w,
                by,
                &c,
                &path,
                &kv,
                &format!(
                    "a staged member is no longer admissible ({})",
                    skips.join("; ")
                ),
            );
        }
        for (i, m) in admitted.iter().enumerate() {
            if let Some(b) = blocked_by.get(&m.id).and_then(|bs| {
                bs.iter()
                    .find(|b| !admitted[..i].iter().any(|x| &x.id == *b))
            }) {
                return discarded(
                    w,
                    by,
                    &c,
                    &path,
                    &kv,
                    &format!("{} is blocked by {b}, which has not landed", m.id),
                );
            }
        }

        let wt = PathBuf::from(kv.get("worktree").unwrap_or(""));
        let staged_head = kv.get("head").unwrap_or("").to_string();
        let head = if kv.get("base") == Some(base_sha.as_str())
            && w.git.rev_parse(&wt, "HEAD").as_deref() == Some(staged_head.as_str())
        {
            staged_head.clone()
        } else {
            match assemble(w, &c, &path, &wt, &base_sha, &admitted, &staged_head) {
                Ok(h) => h,
                Err(why) => {
                    return discarded(
                        w,
                        by,
                        &c,
                        &path,
                        &kv,
                        &format!("the base moved to {base_sha} and {why}"),
                    )
                }
            }
        };

        let now = w.clock.now().to_string();
        let mut round = Kv::default();
        round.push("batch_id", &batch);
        round.push("repo", &repo_name);
        round.push("head", &head);
        round.push("base", &base_sha);
        round.push("members", &render_members(&admitted));
        push_provisional(&mut round, &admitted, &provisional);
        round.push("worktree", &wt.display().to_string());
        round.push("actor", &actor(w));
        round.push("opened", &now);
        let ride = Ride::with(w, &round);
        if let Err((rc, out)) =
            ride.sent(w.lc.promote(&batch, &head, &base_sha, LC_ACTOR, ride.fields()))
        {
            w.err(format!("queue.sh {label}: spira-lc promote refused for {batch} (rc={rc}): {out} — the staged round is left as it was"));
            return FAIL;
        }

        let tree = w
            .git
            .rev_parse(&path, &format!("{head}^{{tree}}"))
            .unwrap_or_default();
        let tested = (kv.get("phase") == Some("green"))
            .then(|| kv.get("tested_tree").unwrap_or("").to_string())
            .filter(|t| !t.is_empty());
        let attested_from = tested.filter(|t| *t == tree).map(|t| {
            (
                t,
                kv.get("head").unwrap_or("").to_string(),
                kv.get("ran").and_then(|r| r.parse().ok()).unwrap_or(1usize),
            )
        });

        if ride.save_rest(w, label, &c, &round).is_err() {
            w.err(format!("queue.sh {label}: {batch} is OPEN on spira-lc but its round record cannot be written"));
            return FAIL;
        }
        let _ = fs::remove_file(c.queue_file(STAGED));
        w.git.branch_delete_sanctioned(&path, &branch_of(&batch));
        mark_running(&c, &batch);
        round_event(
            w,
            &c.s.run,
            "promote",
            &repo_name,
            &batch,
            by,
            &[
                ("head", head.to_string()),
                ("base", base_sha.to_string()),
                ("members", csv(&admitted)),
            ],
        );
        let members: Vec<_> = admitted
            .iter()
            .map(|m| serde_json::json!({"id": m.id, "tip": m.tip}))
            .collect();
        (batch, head, members, attested_from)
    };

    let answer = |attested: bool| {
        w.out(serde_json::json!({"batch": batch, "head": head, "members": members, "attested": attested}).to_string());
    };
    match attested_from {
        Some((tree, tested_head, staged_ran)) => {
            w.out(format!("queue.sh {label}: {batch}'s tree {tree} is the tree its staged pass at {tested_head} judged — attesting that pass"));
            let rc = certify(
                w,
                by,
                &batch,
                Some(&repo_name),
                Some(&head),
                Some(staged_ran),
                None,
                false,
                super::round::LeaseCheck::Inside,
            );
            round_event(
                w,
                &c.s.run,
                "attest",
                &repo_name,
                &batch,
                by,
                &[
                    ("head", head.to_string()),
                    ("tree", tree.to_string()),
                    ("tested", tested_head.to_string()),
                    ("rc", rc.to_string()),
                ],
            );
            if rc == OK {
                answer(true);
            }
            rc
        }
        None => {
            answer(false);
            OK
        }
    }
}

fn discarded(w: &World, by: &str, c: &Ctx, path: &Path, kv: &Kv, why: &str) -> i32 {
    let batch = kv.get("batch_id").unwrap_or("?");
    match discard_staged(w, by, c, path, kv, why) {
        Ok(()) => w.err(format!("queue.sh round promote: staged round {batch} discarded — {why}; cut its members with round open")),
        Err(e) => w.err(format!("queue.sh round promote: {batch} cannot be promoted ({why}) and was not discarded: {e}")),
    }
    FAIL
}

#[cfg(test)]
mod tests {
    use super::refuse_over_cap;
    use state_store::limits::MAX_CUT_MEMBERS;

    #[test_id(t5sbyej)]
    #[test]
    fn a_round_at_the_store_cap_is_admitted_whole_and_one_more_member_is_refused() {
        let ids = |n: usize| (0..n).map(|i| format!("sp-{i}")).collect::<Vec<_>>();
        let (mut at, mut skips) = (ids(MAX_CUT_MEMBERS), Vec::new());
        refuse_over_cap(&mut at, &mut skips, |s| s);
        assert_eq!((at.len(), skips.len()), (MAX_CUT_MEMBERS, 0));
        let mut over = ids(MAX_CUT_MEMBERS + 1);
        refuse_over_cap(&mut over, &mut skips, |s| s);
        assert_eq!(over, ids(MAX_CUT_MEMBERS));
        assert_eq!(
            skips,
            [format!(
                "sp-{MAX_CUT_MEMBERS}: the round is full at {MAX_CUT_MEMBERS} members — not admitted"
            )]
        );
    }
}
