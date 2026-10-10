//! `queue round stage | stage-test | promote | discard`: the next round assembled and fenced
//! behind the one whose suites are still running, tested the moment that one is green, and cut
//! when it lands (STAGED -> OPEN, or STAGED -> DISCARDED). A staged round moves no bead: only
//! `promote` delivers its members, so discarding one returns nothing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::round::{assemble, certify, csv, finish, handle_of, local_ctx, lock, mark_running, phase_of, rounds_dir, set, set_phase, suite_statuses, tail, unjudgeable, BLOCKING, BUILD_RED, LC_ACTOR, RECORD};
use super::{actor, idents, landing_log, lc_cas, read_text, require_lc, Ctx, World, FAIL, OK, USAGE};
use crate::cli::Text;
use crate::ident::bounded_text;
use crate::model::{parse_members, render_members, Member};
use crate::records::{self, one_line, write_atomic, Kv};

pub const STAGED: &str = "round-staged";

fn staged_record(c: &Ctx) -> Option<Kv> {
    records::read_kv(&c.queue_file(STAGED)).ok().flatten()
}

fn branch_of(batch: &str) -> String {
    format!("spira/round-staged/{batch}")
}

fn admit(w: &World, wanted: Vec<Member>) -> (Vec<Member>, BTreeMap<String, Vec<String>>, Vec<String>) {
    let mut skips = Vec::new();
    let mut admitted = Vec::new();
    let mut blocked_by = BTreeMap::new();
    for m in wanted {
        match w.lc.bead_row(&m.id) {
            None => skips.push(format!("{}: no lifecycle row (spira-lc could not say) — not admitted", m.id)),
            Some(r) if r.state != "CERTIFIED" && r.state != "SUBMITTED" => {
                skips.push(format!("{}: lifecycle state={} (not SUBMITTED or CERTIFIED) — not admitted", m.id, r.state))
            }
            Some(r) => match r.tip.filter(|t| !t.is_empty()) {
                Some(tip) if tip.starts_with(&m.tip) => {
                    blocked_by.insert(m.id.clone(), r.blocked_by);
                    admitted.push(Member { id: m.id, tip })
                }
                Some(tip) => skips.push(format!("{}: {} is not its row's tip {tip} — not admitted", m.id, m.tip)),
                None => skips.push(format!("{}: no submitted tip — not admitted", m.id)),
            },
        }
    }
    (admitted, blocked_by, skips)
}

pub fn stage(w: &World, repo: Option<&str>, members_arg: &Text, name: Option<&str>, worktree: Option<&Path>) -> i32 {
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
    let Ok((c, path)) = local_ctx(w, label, repo) else { return FAIL };
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
        if idents(w, label, &[("member id", &m.id)]).is_err() || (!m.tip.is_empty() && idents(w, label, &[("member tip", &m.tip)]).is_err()) {
            return FAIL;
        }
    }
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else { return FAIL };
    let Ok(Some(running)) = records::read_kv(&c.queue_file(RECORD)) else {
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
    let (wanted, in_ahead): (Vec<Member>, Vec<Member>) = wanted.into_iter().partition(|m| !ahead.iter().any(|a| a.id == m.id));
    for m in in_ahead {
        skips.push(format!("{}: already a member of {parent} — not staged", m.id));
    }
    let (admitted, blocked_by, more) = admit(w, wanted);
    skips.extend(more);

    let batch = name.map(str::to_string).unwrap_or_else(|| format!("{repo_name}-{}", w.clock.stamp()));
    let wt = worktree.map(Path::to_path_buf).unwrap_or_else(|| c.s.run.join("worktree").join(format!(".round-staged-{repo_name}")));
    w.git.worktree_prune(&path);
    w.git.worktree_remove(&path, &wt);
    if let Some(p) = wt.parent() {
        let _ = fs::create_dir_all(p);
    }
    if !w.git.worktree_add_detached(&path, &wt, &parent_head) {
        w.err(format!("queue.sh {label}: cannot create the staged worktree {}", wt.display()));
        return FAIL;
    }
    let mut merged: Vec<Member> = Vec::new();
    for m in admitted {
        let blocker = blocked_by.get(&m.id).and_then(|bs| bs.iter().find(|b| !merged.iter().chain(ahead.iter()).any(|x| &x.id == *b)));
        if let Some(b) = blocker {
            skips.push(format!("{}: blocked by {b}, which has not landed and is not merged ahead of it — not staged", m.id));
            continue;
        }
        if super::round::merge_member(w, &c, &wt, &m.id, &m.tip) {
            merged.push(m);
        } else {
            w.git.merge_abort(&wt);
            skips.push(format!("{}: conflicts with the round it would follow", m.id));
        }
    }
    for s in &skips {
        w.out(format!("queue.sh {label}: skip — {s}"));
    }
    if merged.is_empty() {
        w.err(format!("queue.sh {label}: nothing stageable for {repo_name}"));
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }
    let head = w.git.rev_parse(&wt, "HEAD").unwrap_or_default();

    let branch = branch_of(&batch);
    if !w.git.branch_set(&path, &branch, &head, true) {
        w.err(format!("queue.sh {label}: cannot name the staged head {head} as {branch} for the fences"));
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }
    let (rc, out) = w.scripts.gate(&branch, &repo_name, "", "off");
    if rc != 0 {
        w.err(format!("queue.sh {label}: the fences are red on the staged head {head} — not staged\n{}", tail(&out, 20)));
        w.git.worktree_remove(&path, &wt);
        w.git.branch_delete_sanctioned(&path, &branch);
        return FAIL;
    }

    if let Err((rc, out)) = w.lc.stage(&batch, &repo_name, &head, &parent_head, &csv(&merged), &parent, LC_ACTOR) {
        w.err(format!("queue.sh {label}: spira-lc stage refused for {batch} (rc={rc}): {out}"));
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
    kv.push("worktree", &wt.display().to_string());
    kv.push("actor", &actor(w));
    kv.push("opened", &now);
    set_phase(w, &mut kv, "staged");
    if let Err(e) = write_atomic(&c.queue_file(STAGED), &kv.render()) {
        w.err(format!("queue.sh {label}: cannot write the staged record: {e}"));
        return FAIL;
    }
    landing_log(&c.s.run, &format!("QUEUE ROUND-STAGE {now} repo={repo_name} batch={batch} behind={parent} head={head} members={}", csv(&merged)));
    w.out(format!("batch={batch}"));
    w.out(format!("behind={parent}"));
    w.out(format!("head={head}"));
    w.out(format!("members={}", render_members(&merged)));
    OK
}

fn results_dir(c: &Ctx, batch: &str) -> PathBuf {
    rounds_dir(c).join(format!("{batch}.results"))
}

pub fn stage_test(w: &World, repo: Option<&str>) -> i32 {
    let label = "round stage-test";
    let Ok((c, path)) = local_ctx(w, label, repo) else { return FAIL };
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let (batch, head, wt) = {
        let Ok(_g) = lock(w, label, &c) else { return FAIL };
        let Some(mut kv) = staged_record(&c) else {
            w.err(format!("queue.sh {label}: no staged round for {}", c.r.name));
            return FAIL;
        };
        let parent = kv.get("parent").unwrap_or("").to_string();
        match records::read_kv(&c.queue_file(RECORD)) {
            Ok(Some(run)) if run.get("batch_id") == Some(parent.as_str()) && phase_of(w, &parent) == "green" => {}
            Ok(Some(run)) if run.get("batch_id") == Some(parent.as_str()) => {
                w.err(format!("queue.sh {label}: {parent} is {}, not green — the staged round is tested once it is", phase_of(w, &parent)));
                return FAIL;
            }
            _ => {
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
        set_phase(w, &mut kv, "testing");
        kv.remove("red");
        kv.remove("tested_tree");
        if let Err(e) = write_atomic(&c.queue_file(STAGED), &kv.render()) {
            w.err(format!("queue.sh {label}: cannot write the staged record: {e}"));
            return FAIL;
        }
        (kv.get("batch_id").unwrap_or("").to_string(), head, wt)
    };

    let Some(base) = c.r.landref.clone() else {
        w.err(format!("queue.sh {label}: cannot resolve the landing ref of {}", c.r.name));
        return FAIL;
    };
    let results = results_dir(&c, &batch);
    let _ = fs::remove_dir_all(&results);
    let _ = fs::create_dir_all(rounds_dir(&c));
    let out = w.scripts.round_vm(&wt, &results, &base, (&batch, &c.r.name), c.s.round_wall_secs, &handle_of(&c, &batch));
    let _ = fs::remove_file(handle_of(&c, &batch));
    let mut found = Vec::new();
    suite_statuses(&results, &mut found, 0);
    let mut reds: Vec<String> = Vec::new();
    let fault = match out.rc {
        0 | 1 if found.is_empty() => Some("round-vm ran and left no verdicts — a fault of the round machinery, not of the candidates".to_string()),
        0 | 1 => unjudgeable(&wt, &found),
        4 => {
            reds.push(BUILD_RED.into());
            None
        }
        124 | 137 => Some(format!("round-vm exceeded the {}s wall", c.s.round_wall_secs)),
        rc => Some(format!("round-vm exited {rc}")),
    };
    reds.extend(found.iter().filter(|(_, s)| BLOCKING.contains(&s.as_str())).map(|(n, _)| n.clone()));
    reds.sort();
    reds.dedup();

    let Ok(_g) = lock(w, label, &c) else { return FAIL };
    let Some(mut kv) = staged_record(&c) else {
        w.err(format!("queue.sh {label}: the staged round {batch} was promoted or discarded while it was tested — the result is dropped"));
        return FAIL;
    };
    if kv.get("batch_id") != Some(batch.as_str()) || kv.get("phase") != Some("testing") || kv.get("head") != Some(head.as_str()) {
        w.err(format!("queue.sh {label}: the staged round {batch} changed while it was tested — the result is dropped"));
        return FAIL;
    }
    if let Some(why) = fault {
        set_phase(w, &mut kv, "fault");
        let _ = write_atomic(&c.queue_file(STAGED), &kv.render());
        w.err(format!("queue.sh {label}: {why}; the staged round is not judged\n{}", tail(&out.err, 20)));
        return super::round::FAULT;
    }
    if !reds.is_empty() {
        set(&mut kv, "red", &reds.join(","));
        set_phase(w, &mut kv, "red");
        let _ = write_atomic(&c.queue_file(STAGED), &kv.render());
        w.out(format!("queue.sh {label}: staged round {batch} is RED at {head}: {}", reds.join(",")));
        w.out(format!("red={}", reds.join(",")));
        return FAIL;
    }
    let Some(tree) = w.git.rev_parse(&path, &format!("{head}^{{tree}}")) else {
        w.err(format!("queue.sh {label}: cannot resolve {head}^{{tree}} — nothing can say what was judged"));
        return FAIL;
    };
    set(&mut kv, "tested_tree", &tree);
    set_phase(w, &mut kv, "green");
    if let Err(e) = write_atomic(&c.queue_file(STAGED), &kv.render()) {
        w.err(format!("queue.sh {label}: cannot write the staged record: {e}"));
        return FAIL;
    }
    landing_log(&c.s.run, &format!("QUEUE ROUND-STAGE-GREEN {} repo={} batch={batch} head={head} tree={tree}", w.clock.now(), c.r.name));
    w.out(format!("queue.sh {label}: staged round {batch} is GREEN at {head} (tree {tree})"));
    OK
}

/// Drop the staged round: its spira-lc row, record, worktree and fence branch. The caller holds
/// the lock. A row spira-lc no longer holds STAGED (already terminal, or never written) is gone
/// already; one that has been promoted is not ours to discard.
pub(super) fn discard_staged(w: &World, c: &Ctx, path: &Path, kv: &Kv, reason: &str) -> Result<(), String> {
    let batch = kv.get("batch_id").unwrap_or("").to_string();
    let why = bounded_text(reason);
    let event = serde_json::json!({"Discard": {"reason": why}}).to_string();
    match w.lc.batch_state(&batch) {
        Some((state, _)) if state == "STAGED" => {
            lc_cas(w, &batch, |s, v| w.lc.batch_event(&batch, s, v, LC_ACTOR, &event)).map_err(|(rc, out)| format!("spira-lc refused to discard {batch} (rc={rc}): {out}"))?;
        }
        Some((state, _)) if !matches!(state.as_str(), "LANDED" | "SETTLED" | "ABANDONED" | "DISCARDED") => {
            return Err(format!("spira-lc has {batch} {state}, not STAGED — it is not discarded"));
        }
        _ => {}
    }
    let _ = fs::remove_file(c.queue_file(STAGED));
    w.git.worktree_remove(path, &PathBuf::from(kv.get("worktree").unwrap_or("")));
    w.git.branch_delete_sanctioned(path, &branch_of(&batch));
    finish(c, &batch, &format!("discarded: {}", one_line(reason)));
    landing_log(&c.s.run, &format!("QUEUE ROUND-DISCARD {} repo={} batch={batch} reason={}", w.clock.now(), c.r.name, one_line(reason)));
    Ok(())
}

/// The round `parent` can no longer be followed: discard the round staged behind it, if any.
pub(super) fn discard_behind(w: &World, c: &Ctx, path: &Path, parent: &str, reason: &str) {
    let Some(kv) = staged_record(c) else { return };
    if kv.get("parent") != Some(parent) {
        return;
    }
    match discard_staged(w, c, path, &kv, reason) {
        Ok(()) => w.out(format!("queue.sh: staged round {} discarded — {reason}", kv.get("batch_id").unwrap_or("?"))),
        Err(e) => w.err(format!("queue.sh: the staged round {} behind {parent} was not discarded: {e}", kv.get("batch_id").unwrap_or("?"))),
    }
}

pub fn discard(w: &World, repo: Option<&str>, reason: &Text) -> i32 {
    let label = "round discard";
    let reason = read_text(w, reason).unwrap_or_default();
    if reason.trim().is_empty() {
        w.err(format!("queue.sh {label}: --reason is required — pass --reason \"<why>\" so the discard is auditable"));
        return USAGE;
    }
    let Ok((c, path)) = local_ctx(w, label, repo) else { return FAIL };
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else { return FAIL };
    let Some(kv) = staged_record(&c) else {
        w.err(format!("queue.sh {label}: no staged round for {}", c.r.name));
        return FAIL;
    };
    match discard_staged(w, &c, &path, &kv, &reason) {
        Ok(()) => {
            w.out(format!("queue.sh {label}: staged round {} discarded", kv.get("batch_id").unwrap_or("?")));
            OK
        }
        Err(e) => {
            w.err(format!("queue.sh {label}: {e}"));
            FAIL
        }
    }
}

pub fn promote(w: &World, repo: Option<&str>) -> i32 {
    let label = "round promote";
    let Ok((c, path)) = local_ctx(w, label, repo) else { return FAIL };
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let repo_name = c.r.name.clone();
    let (batch, head, attested_from) = {
        let Ok(_g) = lock(w, label, &c) else { return FAIL };
        let Some(kv) = staged_record(&c) else {
            w.err(format!("queue.sh {label}: no staged round for {repo_name}"));
            return FAIL;
        };
        let batch = kv.get("batch_id").unwrap_or("").to_string();
        let parent = kv.get("parent").unwrap_or("").to_string();
        if let Ok(Some(running)) = records::read_kv(&c.queue_file(RECORD)) {
            w.err(format!(
                "queue.sh {label}: round {} is still open for {repo_name} (phase {}) — {batch} follows {parent} and is promoted once it has landed",
                running.get("batch_id").unwrap_or("?"),
                phase_of(w, running.get("batch_id").unwrap_or(""))
            ));
            return FAIL;
        }
        let parent_state = w.lc.batch_state(&parent).map(|(s, _)| s).unwrap_or_default();
        if parent_state != "LANDED" {
            let why = format!("the round it followed, {parent}, is {}, not LANDED", if parent_state.is_empty() { "unknown" } else { parent_state.as_str() });
            return discarded(w, &c, &path, &kv, &why);
        }
        let Some(base) = c.r.landref.clone() else {
            w.err(format!("queue.sh {label}: cannot resolve landing ref for {repo_name}"));
            return FAIL;
        };
        let Some(base_sha) = w.git.rev_parse(&path, &base) else {
            w.err(format!("queue.sh {label}: cannot resolve {base}"));
            return FAIL;
        };

        let staged_members = kv.members();
        let (admitted, blocked_by, skips) = admit(w, staged_members.clone());
        let tips_kept = admitted.len() == staged_members.len() && admitted.iter().zip(&staged_members).all(|(a, s)| a.tip == s.tip);
        if !tips_kept {
            return discarded(w, &c, &path, &kv, &format!("a staged member is no longer admissible ({})", skips.join("; ")));
        }
        for (i, m) in admitted.iter().enumerate() {
            if let Some(b) = blocked_by.get(&m.id).and_then(|bs| bs.iter().find(|b| !admitted[..i].iter().any(|x| &x.id == *b))) {
                return discarded(w, &c, &path, &kv, &format!("{} is blocked by {b}, which has not landed", m.id));
            }
        }

        let wt = PathBuf::from(kv.get("worktree").unwrap_or(""));
        let staged_head = kv.get("head").unwrap_or("").to_string();
        let head = if kv.get("base") == Some(base_sha.as_str()) && w.git.rev_parse(&wt, "HEAD").as_deref() == Some(staged_head.as_str()) {
            staged_head.clone()
        } else {
            match assemble(w, &c, &path, &wt, &base_sha, &admitted, &staged_head) {
                Ok(h) => h,
                Err(why) => return discarded(w, &c, &path, &kv, &format!("the base moved to {base_sha} and {why}")),
            }
        };

        if let Err((rc, out)) = w.lc.promote(&batch, &head, &base_sha, LC_ACTOR) {
            w.err(format!("queue.sh {label}: spira-lc promote refused for {batch} (rc={rc}): {out} — the staged round is left as it was"));
            return FAIL;
        }

        let tree = w.git.rev_parse(&path, &format!("{head}^{{tree}}")).unwrap_or_default();
        let tested = (kv.get("phase") == Some("green")).then(|| kv.get("tested_tree").unwrap_or("").to_string()).filter(|t| !t.is_empty());
        let attested_from = tested.filter(|t| *t == tree).map(|t| (t, kv.get("head").unwrap_or("").to_string()));

        let now = w.clock.now().to_string();
        let mut round = Kv::default();
        round.push("batch_id", &batch);
        round.push("repo", &repo_name);
        round.push("head", &head);
        round.push("base", &base_sha);
        round.push("members", &render_members(&admitted));
        round.push("worktree", &wt.display().to_string());
        round.push("actor", &actor(w));
        round.push("opened", &now);
        if let Err(e) = write_atomic(&c.queue_file(RECORD), &round.render()) {
            w.err(format!("queue.sh {label}: {batch} is OPEN on spira-lc but its round record cannot be written: {e}"));
            return FAIL;
        }
        let _ = fs::remove_file(c.queue_file(STAGED));
        w.git.branch_delete_sanctioned(&path, &branch_of(&batch));
        mark_running(&c, &batch);
        landing_log(&c.s.run, &format!("QUEUE ROUND-PROMOTE {now} repo={repo_name} batch={batch} head={head} base={base_sha} members={}", csv(&admitted)));
        w.out(format!("batch={batch}"));
        w.out(format!("head={head}"));
        w.out(format!("members={}", render_members(&admitted)));
        (batch, head, attested_from)
    };

    match attested_from {
        Some((tree, tested_head)) => {
            w.out(format!("queue.sh {label}: {batch}'s tree {tree} is the tree its staged pass at {tested_head} judged — attesting that pass"));
            let rc = certify(w, &batch, Some(&repo_name), Some(&head));
            landing_log(&c.s.run, &format!("QUEUE ROUND-ATTEST {} repo={repo_name} batch={batch} head={head} tree={tree} tested={tested_head} rc={rc}", w.clock.now()));
            if rc == OK {
                w.out("attested=1");
            }
            rc
        }
        None => {
            w.out("attested=0");
            OK
        }
    }
}

fn discarded(w: &World, c: &Ctx, path: &Path, kv: &Kv, why: &str) -> i32 {
    let batch = kv.get("batch_id").unwrap_or("?");
    match discard_staged(w, c, path, kv, why) {
        Ok(()) => w.err(format!("queue.sh round promote: staged round {batch} discarded — {why}; cut its members with round open")),
        Err(e) => w.err(format!("queue.sh round promote: {batch} cannot be promoted ({why}) and was not discarded: {e}")),
    }
    FAIL
}
