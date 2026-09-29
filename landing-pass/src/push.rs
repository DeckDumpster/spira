//! push and hold (DESIGN.md §4.3, §4.5): rebase (re-cut on conflict), confine, gate, and
//! then either land in a private worktree and push (push) or note the bead and leave the
//! base alone (hold).

use crate::model::{BeadRow, GateOutcome, LandMode};
use crate::pass::{first_nonempty, head1, or, Flow, Pass, Walk};
use crate::util::tail_lines;

pub(crate) fn push_or_hold(p: &Pass, w: &Walk, br: &str, id: &str, bead: &BeadRow) -> Flow {
    let repo = w.repo;
    let name = &repo.name;
    let path = &repo.path;
    let mut tip = p.git.rev_parse(path, br).unwrap_or_default();

    // A hold-mode branch already sent at this tip is left alone (it has no PR to refresh
    // and no remote to push to).
    if repo.mode == LandMode::Hold {
        if let Some(sub) = p.files.submitted(id) {
            if sub.settles(&tip, p.clock.now()) {
                return Flow::Next;
            }
        }
    }

    // A known-stuck pair is not re-attempted: has the tip or the base moved since the mark?
    let cur_base = p.git.rev_parse(path, &w.base_fq).unwrap_or_default();
    let ls = p.files.land_state(id).unwrap_or_default();
    let no_rebase_mark = format!("no-rebase@{cur_base}");
    if ls.state == "RED" && ls.tip == tip && ls.reason == no_rebase_mark {
        p.out.log(&format!("CHECK6 {id}: tip and base unchanged since last RED mark — skipping repeat rebase attempt"));
        return Flow::Next;
    }

    let rb = p.lib.rebase(br, &w.base_fq, path, name);
    if !rb.ok {
        // ONLY A CONFLICT MAY REOPEN: the other failures are the pass failing to ask.
        if rb.failure != "conflict" {
            p.out.log(&format!(
                "CHECK6 {id}: could not attempt a rebase of {br} onto {} ({}) — not a conflict, leaving the bead closed",
                w.base,
                or(&rb.failure, "unknown")
            ));
            if rb.failure == "rebase-refused" {
                p.lib.ask_rebase_refused(id, br, name, or(&rb.refused_reason, "unknown"));
            }
            return Flow::Next;
        }
        if p.lib.pr_merged(path, br) {
            p.out.log(&format!("CHECK6 {id}: {br} does not rebase onto {}, but its pull request is merged — landed, not stuck", w.base));
            return Flow::Next;
        }
        if ls.state == "RED" && ls.tip == tip {
            p.out.log(&format!("CHECK6 {id}: tip unchanged since last RED mark — skipping duplicate bump"));
            if ls.reason != no_rebase_mark {
                p.lib.land_mark(id, "RED", &tip, &no_rebase_mark);
            }
            return Flow::Next;
        }
        p.lib.bump_requeue(id, "merge-conflict");
        let n = p.lib.requeues_of(id);
        let n_s = n.max(1).to_string();
        let path_s = path.to_string_lossy().into_owned();
        let note = p.lib.conflict_note(&[&path_s, br, &w.base_fq, name, &rb.conflicts, "sentinel", &n_s]);
        let others = p.lib.other_beads(path, br, &w.base_fq, &rb.conflicts);
        let rc = p.lib.recut(br, &w.base_fq, path, name);
        if !rc.ok {
            let prev_class = ls.reason.split('@').next().unwrap_or("");
            if rc.applied > 0 {
                let conflicts = first_nonempty(&[&rc.conflicts, &rb.conflicts, "unknown"]);
                p.lib.ask_rebase_loop(&[id, br, name, &n_s, conflicts, &others, &path_s, &w.base_fq]);
                p.out.progress(&format!(
                    "escalated {id} — re-cut conflicted on {br} after {n_s} attempt(s); {} commit(s) moved to {}",
                    rc.applied, w.base
                ));
            } else if ls.state == "RED" && prev_class == "no-rebase" {
                let at = if ls.at == 0 { "0".to_string() } else { ls.at.to_string() };
                p.lib.ask_red_recurring(id, br, name, "no-rebase", &at);
                p.out.progress(&format!("escalated {id} — recurring no-rebase on {br} after {n_s} attempt(s)"));
            } else if n >= p.s.rebase_escalate_at {
                // The main walk reopens even on escalation, or the ask is about a bead
                // nothing can ever work.
                p.lib.reopen(id, "rebase-conflict", &note);
                p.lib.ask_rebase_loop(&[id, br, name, &n_s, or(&rb.conflicts, "unknown"), &others, &path_s, &w.base_fq]);
                p.out.progress(&format!("escalated {id} — rebase conflict x{n} on {br}"));
            } else {
                p.lib.reopen(id, "rebase-conflict", &note);
                p.out.progress(&format!("reopened {id} — does not rebase onto {}", w.base));
                p.lib.event(
                    "bead.reopened",
                    id,
                    &format!("reopened {id} — {br} does not rebase onto {} in {name}", w.base),
                    &format!("conflicts in {}; the next aeon is handed the rebase", or(&rb.conflicts, "unknown")),
                );
            }
            let t = p.git.rev_parse(path, br).unwrap_or_default();
            p.lib.land_mark(id, "RED", &t, &no_rebase_mark);
            return Flow::Next;
        }
        p.out.log(&format!("CHECK6 {id}: re-cut {br} onto {} ({} commit(s)) — falling through to gate", w.base, rc.applied));
    }
    tip = p.git.rev_parse(path, br).unwrap_or_default();
    w.judge(br);

    // CONFINEMENT BEFORE THE GATE: "is this branch allowed to land at all".
    let labels = bead.labels.join(" ");
    let (crc, cout) = p.tools.confine(id, br, path, &w.base_fq, &labels);
    if crc == 1 {
        p.lib.reopen(id, "confine-fail", &format!("Reopened by sentinel: {cout}"));
        p.out.progress(&format!("reopened {id} — spike branch is not confined to its document"));
        p.out.log(&format!("CHECK6 {id}: {}", head1(&cout)));
        p.lib.land_mark(id, "RED", &tip, "confine");
        w.unjudge(br);
        return Flow::Next;
    } else if crc != 0 {
        p.out.log(&format!("CHECK6 {id}: {br} — confine.sh could not evaluate: {}", head1(&cout)));
        return Flow::Next;
    }

    if !p.budget_allows(bead, name, id) {
        return Flow::BudgetCut;
    }
    let g = p.run_gate(name, br, id, false, &tip);
    if g.outcome != GateOutcome::Pass {
        let reason = g.reason_or("unspecified");
        p.out.log(&format!("CHECK6 {id}: gate {} on {br} in {name} ({reason})", g.outcome.word()));
        p.lib.land_mark(id, "GATED", &tip, &format!("{}:{reason}", g.outcome.word()));
        match g.outcome {
            GateOutcome::BaseFail => {
                p.out.log(&format!(
                    "CHECK6 {id}: gate: held — the base fails its own gate; {name}'s gate is red against {} too (suite {})",
                    w.base, g.suite
                ));
                p.base_fail(w, br, id, bead, &tip, &g);
            }
            GateOutcome::NoVerdict => p.lib.noverdict(id, br, name, &reason, g.outcome.word(), &g.out),
            _ => {
                let (st_closed, st) = {
                    let st = p.beads.land_status(id);
                    (st == "closed", st)
                };
                if !st_closed {
                    p.out.log(&format!("CHECK6 {id}: bead is now {st} (was closed at scan time) — not reopening {br}"));
                    return Flow::Next;
                }
                let n = p.count_or_q(path, &format!("{}..{br}", w.base));
                let note = format!(
                    "Reopened by sentinel: branch {br} failed {name}'s landing gate. The branch carries {n} commit(s) from the previous session — the next aeon should resume from the existing work, not restart.\n\n{}",
                    tail_lines(&g.out, 20)
                );
                p.lib.reopen(id, "gate-red", &note);
                p.out.progress(&format!("reopened {id} — failed the gate"));
                p.lib.event("bead.reopened", id, &format!("reopened {id} — {br} failed {name}'s landing gate"), &tail_lines(&g.out, 3));
                p.lib.land_mark(id, "RED", &tip, "gate");
                w.unjudge(br);
            }
        }
        return Flow::Next;
    }
    if g.reason.as_deref() == Some("cached") {
        p.out.log(&format!("CHECK6 {id}: gate PASS on {br} in {name} — this tree had already passed, so no suite ran"));
    }
    p.files.clear_noverdict(br);
    let st = p.beads.land_status(id);
    if st != "closed" {
        p.out.log(&format!("CHECK6 {id}: bead is now {st} (was closed at scan time) — not landing {br}"));
        return Flow::Next;
    }

    if repo.mode == LandMode::Hold {
        p.lib.note(
            id,
            &format!(
                "Gated and held: {br} passed {name}'s landing gate. Spira does not advance {name}'s {}. Merge it by hand when you are ready — nothing else will.",
                repo.base_branch
            ),
        );
        p.files.mark_submitted(id, &tip, "hold", p.clock.now());
        p.out.log(&format!("gated and held {br} in {name} — nothing here advances {}", w.base));
        return Flow::Next;
    }
    land(p, w, br, id, tip);
    Flow::Next
}

enum Landed {
    Wedged,
    NoRebase(String),
    Nothing,
    Pushed,
    MergedUnpushed { blocked: bool },
    Conflict(String),
}

/// The push land (DESIGN.md §4.5): the branch's OWN commits merged into the base in a
/// private worktree and pushed; a lost race fetches, replays the BRANCH and tries again.
fn land(p: &Pass, w: &Walk, br: &str, id: &str, mut tip: String) {
    let repo = w.repo;
    let name = &repo.name;
    let path = &repo.path;
    if !p.git.tree_ok(&w.land) {
        p.out.log(&format!("CHECK6 {id}: no landing worktree at {} — leaving {br} to the next pass", w.land.display()));
        return;
    }
    // THE LIFECYCLE SWITCH: ON, spira-lc is authoritative for the delivery this landing is,
    // so an unreachable machine refuses the landing rather than land it unrecorded.
    if p.s.lifecycle_enforce {
        if let Err(why) = p.lc_ready() {
            p.out.log(&crate::lifecycle::unreachable_line(&why, &format!("not landing {br} this pass")));
            return;
        }
    }
    let lc = p.s.lifecycle_enforce;
    let remote = repo.base_remote.clone().unwrap_or_default();
    let subject = p.lib.land_subject(id);
    let mut outcome = Landed::Nothing;
    for attempt in 1..=3u64 {
        if !p.git.tree_checkout_landing(&w.land, &w.base_fq) {
            outcome = Landed::Wedged;
            break;
        }
        let pre = p.git.tree_head(&w.land);
        if let Err(files) = p.git.tree_merge(&w.land, &subject, br, &p.s.git_name, &p.s.git_email) {
            outcome = Landed::Conflict(files);
            break;
        }
        // A merge that does not move HEAD is a no-op: the base already holds the branch.
        if p.git.tree_head(&w.land) == pre {
            outcome = Landed::Nothing;
            break;
        }
        match p.lib.push(&w.land, &remote, &format!("landing:{}", repo.base_branch)) {
            Ok(()) => {
                outcome = Landed::Pushed;
                break;
            }
            Err(msg) => {
                let first = or(head1(&msg), "unknown").to_string();
                let race = msg.contains("non-fast-forward") || msg.contains("fetch first") || msg.contains("rejected");
                if !race {
                    p.out.log(&format!("landing: push failed for {br}: {first}"));
                    outcome = Landed::MergedUnpushed { blocked: true };
                    break;
                }
                // A keyword is not proof of a lost race: did the base actually move?
                let rref = format!("refs/remotes/{remote}/{}", repo.base_branch);
                let before = p.git.rev_parse(path, &rref);
                p.git.fetch(path, &remote);
                let after = p.git.rev_parse(path, &rref);
                if before == after {
                    p.out.log(&format!("landing: push failed for {br} — rejected but {} did not move ({first})", w.base));
                    outcome = Landed::MergedUnpushed { blocked: true };
                    break;
                }
                p.out.log(&format!("landing: push rejected, {} moved — retry {attempt}", w.base));
                p.clock.sleep(attempt);
                let rb = p.lib.rebase(br, &w.base_fq, path, name);
                if !rb.ok {
                    outcome = if rb.failure == "conflict" {
                        Landed::Conflict(rb.conflicts.clone())
                    } else {
                        Landed::NoRebase(or(&rb.failure, "unknown").to_string())
                    };
                    break;
                }
                tip = p.git.rev_parse(path, br).unwrap_or_default();
                if p.git.content_landed(path, br, &w.base_fq) {
                    outcome = Landed::Nothing;
                    break;
                }
                outcome = Landed::MergedUnpushed { blocked: false };
            }
        }
    }
    match outcome {
        Landed::Wedged => p.out.log(&format!(
            "CHECK6 {id}: landing worktree at {} will not check out {} — leaving {br} to the next pass",
            w.land.display(),
            w.base
        )),
        Landed::NoRebase(why) => p.out.log(&format!(
            "CHECK6 {id}: the retry could not attempt a rebase of {br} onto {} ({why}) — not a conflict, leaving the bead closed",
            w.base
        )),
        Landed::Nothing => {
            p.out.log(&format!("CHECK6 {id}: {br} adds nothing to {} once rebased — its work is already there, nothing to land", w.base))
        }
        Landed::Pushed => {
            p.out.progress(&format!("landed {br}"));
            // RECORDED FIRST: everything after can fail; the fact that must survive is that
            // this commit is on the base (law-closed-is-not-landed, one layer in).
            p.lib.land_mark(id, "LANDED", &tip, name);
            p.files.drop_ejected(id);
            let head = p.git.tree_head(&w.land).unwrap_or_default();
            if lc {
                p.lib.deliver_delivered(id, &head);
            }
            p.lib.closeout(id, &head, path);
            p.lib.close_on_land(id, &head);
            let short: String = head.chars().take(7).collect();
            p.lib.event("bead.landed", id, &format!("landed {br} on {name}'s {}", w.base), &format!("merged as {short} from {tip}"));
            w.unjudge(br);
            w.landed_any.set(true);
        }
        Landed::MergedUnpushed { blocked } => {
            p.git.tree_reset_hard(&w.land, &w.base_fq);
            if blocked {
                p.out.log(&format!("landing: {br} merges clean but push failed — leaving closed"));
            } else {
                p.out.log(&format!("landing: {br} merges clean but push kept losing the race — retrying next pass"));
                if lc {
                    p.lib.deliver_requeued(id, &tip);
                }
            }
        }
        Landed::Conflict(files) => {
            p.git.tree_merge_abort(&w.land);
            // FETCH BEFORE ASSERTING A NEGATIVE; then ask the commit graph whether this is
            // already landed before reopening.
            if !remote.is_empty() {
                p.git.fetch(path, &remote);
            }
            let ahead = p.count_or_q(path, &format!("{}..{br}", w.base_fq));
            let anc = if p.git.is_ancestor(path, br, &w.base_fq) { "yes" } else { "no" };
            if anc == "yes" || ahead == "0" || ahead == "?" {
                p.out.log(&format!(
                    "landing: {br} introduces nothing new to {} (ancestor={anc}, commits-ahead={ahead}) — landed, not conflicted; not reopening {id}",
                    w.base
                ));
                p.lib.event(
                    "bead.landed",
                    id,
                    &format!("landed {br} on {name}'s {}", w.base),
                    &format!("branch introduces no commit {} lacks; a conflict here means already-merged", w.base),
                );
                w.unjudge(br);
                return;
            }
            p.out.log(&format!("landing: {br} genuinely conflicts with {} (ancestor={anc}, commits-ahead={ahead})", w.base));
            let others = p.lib.other_beads(path, br, &w.base_fq, &files);
            let note = if others.is_empty() {
                format!("Reopened by sentinel: branch {br} conflicts with {}. The branch carries {ahead} commit(s) from the previous session — rebase onto {}, resolve the conflict, and finish. A merge conflict is not an escalation.", w.base, w.base)
            } else {
                format!("Reopened by sentinel: branch {br} conflicts with {}. The branch carries {ahead} commit(s) from the previous session. Those files were changed on {} by {others} — check whether this work is already landed before resolving.", w.base, w.base)
            };
            if lc {
                p.lib.deliver_returned(id, &format!("genuinely conflicts with {}", w.base));
            }
            p.lib.reopen(id, "rebase-conflict", &note);
            p.out.progress(&format!("reopened {id} — branch conflicts with {}", w.base));
            p.lib.event(
                "bead.reopened",
                id,
                &format!("reopened {id} — {br} conflicts with {name}'s {}", w.base),
                "the merge would not apply; rebase and finish",
            );
            w.unjudge(br);
        }
    }
}
