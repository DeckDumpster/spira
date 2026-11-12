//! `hand-land`: the one way a person lands out of turn. A landing moves the landing ref under
//! any open round, and the round's next pass then builds a tree it was not cut for; this verb
//! shows what that costs, then either waits the round out or voids it, and lands, activates
//! and reads through the new release in one act.

use std::path::Path;

use super::round::{abandon_for_landing, live_records, phase_of};
use super::{
    idents, landing_log, read_text, repo_path, require_lc, resolve, simple, Ctx, World,
    DEPLOY_FAULT, FAIL, OK, ROUND_OPEN, USAGE,
};
use crate::cli::Text;
use crate::model::{LandMode, Member};
use crate::records::one_line;

const POLL_SECS: u64 = 30;
const CONF_DIR: &str = "spira/conf.d/";
const HOLD_MARGIN_SECS: u64 = 900;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
    Wait,
    Subvert,
}

impl Policy {
    pub fn parse(s: &str) -> Option<Policy> {
        match s {
            "wait" => Some(Policy::Wait),
            "subvert" => Some(Policy::Subvert),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Policy::Wait => "wait",
            Policy::Subvert => "subvert",
        }
    }
}

pub struct Args<'a> {
    pub bead: &'a str,
    pub repo: Option<&'a str>,
    pub tip: &'a str,
    pub by: &'a str,
    pub reason: &'a Text,
    pub evidence: &'a Text,
    pub evidence_tip: &'a str,
    pub round: Option<Policy>,
    pub dry_run: bool,
    pub worktree: Option<&'a Path>,
}

struct OpenRound {
    batch: String,
    base: String,
    phase: String,
    minutes: u64,
    members: Vec<Member>,
    greens: usize,
}

fn open_rounds(w: &World, c: &Ctx, path: &Path) -> Result<Vec<OpenRound>, String> {
    Ok(live_records(w, c)?
        .into_iter()
        .map(|kv| {
            let batch = kv.get("batch_id").unwrap_or("?").to_string();
            let head = kv.get("head").unwrap_or("");
            OpenRound {
                phase: phase_of(w, &batch),
                minutes: w
                    .clock
                    .now()
                    .saturating_sub(kv.get("opened").and_then(|v| v.parse().ok()).unwrap_or(0))
                    / 60,
                members: kv.members(),
                greens: usize::from(round_green(w, c, path, head)),
                base: kv.get("base").unwrap_or("").to_string(),
                batch,
            }
        })
        .collect())
}

fn round_green(w: &World, c: &Ctx, path: &Path, head: &str) -> bool {
    let Some(tree) = w.git.rev_parse(path, &format!("{head}^{{tree}}")) else {
        return false;
    };
    gate::cert::path(&super::land::verdicts_dir(w, &c.s), &c.r.name, &tree)
        .and_then(|p| w.env.read_file(&p).ok())
        .and_then(|text| gate::cert::certifies(&text, &c.r.name, &tree))
        .is_some_and(|cert| matches!(cert.source, gate::cert::Source::Round))
}

fn ids(members: &[Member]) -> String {
    members
        .iter()
        .map(|m| m.id.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

fn names(rounds: &[OpenRound]) -> String {
    rounds
        .iter()
        .map(|r| r.batch.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// `land-local` called directly under an open round: refused, naming the verb that lands it.
/// `None` when no round is open for the repository, or it is not one this refusal covers.
pub fn refuse_direct(w: &World, repo: Option<&str>) -> Option<i32> {
    let c = resolve(w, "land-local", repo).ok()?;
    if c.r.mode != LandMode::QueueLocal {
        return None;
    }
    let path = repo_path(w, "land-local", &c).ok()?;
    let rounds = match open_rounds(w, &c, &path) {
        Ok(r) => r,
        Err(e) => {
            w.err(format!("queue.sh land-local: cannot tell whether a round is open for {}: {e} — refused, nothing changed", c.r.name));
            return Some(FAIL);
        }
    };
    let first = rounds.first()?;
    w.err(format!(
        "queue.sh land-local: a round is open on {} ({}, phase {}, members {}): landing now voids it — refused, nothing changed; land with `queue hand-land <bead> --round wait|subvert` (add --dry-run to see the cost first)",
        first.base, names(&rounds), first.phase, ids(&first.members)
    ));
    Some(ROUND_OPEN)
}

fn config_keys(w: &World, path: &Path, base_sha: &str, tip: &str) -> (Vec<String>, Vec<String>) {
    let changed = w.git.numstat(path, base_sha, tip).unwrap_or_default();
    let (mut added, mut retired) = (Vec::new(), Vec::new());
    for (file, _, _) in changed {
        let Some(key) = file.strip_prefix(CONF_DIR) else {
            continue;
        };
        match (w.git.path_exists(path, base_sha, &file), w.git.path_exists(path, tip, &file)) {
            (false, true) => added.push(key.to_string()),
            (true, false) => retired.push(key.to_string()),
            _ => {}
        }
    }
    (added, retired)
}

fn list(v: &[String]) -> String {
    if v.is_empty() {
        "none".into()
    } else {
        v.join(",")
    }
}

fn consequence(rounds: &[OpenRound], policy: Option<Policy>) -> String {
    if rounds.is_empty() {
        return "no round open".into();
    }
    if policy == Some(Policy::Wait) {
        return format!("landing waits for round {}", names(rounds));
    }
    let members: usize = rounds.iter().map(|r| r.members.len()).sum();
    let greens: usize = rounds.iter().map(|r| r.greens).sum();
    format!(
        "round {} will be voided: {members} member(s) re-queued, {greens} green pass(es) discarded",
        names(rounds)
    )
}

struct HoldGuard<'a> {
    c: &'a Ctx,
    pid: u32,
}

impl Drop for HoldGuard<'_> {
    fn drop(&mut self) {
        spira_config::cut_hold::lift(&self.c.s.queue_dir, &self.c.r.name, self.pid);
    }
}

fn log(w: &World, c: &Ctx, a: &Args, policy: Policy, voided: &str, verify: &str) {
    landing_log(
        &c.s.run,
        &format!(
            "QUEUE HAND-LAND {} repo={} bead={} tip={} by={} round={} voided={voided} verify={} reason={}",
            w.clock.now(),
            c.r.name,
            a.bead,
            a.tip,
            a.by,
            policy.word(),
            one_line(verify),
            one_line(&read_text(w, a.reason).unwrap_or_default())
        ),
    );
}

pub fn hand_land(w: &World, a: &Args) -> i32 {
    let label = "hand-land";
    let (reason, evidence) = match (read_text(w, a.reason), read_text(w, a.evidence)) {
        (Ok(r), Ok(e)) => (r, e),
        (Err(e), _) | (_, Err(e)) => {
            w.err(format!("queue.sh {label}: cannot read the reason or evidence: {e}"));
            return USAGE;
        }
    };
    if !a.dry_run {
        for (word, text) in [("--reason", &reason), ("--evidence", &evidence)] {
            if text.trim().is_empty() {
                w.err(format!("queue.sh {label}: {word} is required"));
                return USAGE;
            }
        }
        if a.round.is_none() {
            w.err(format!("queue.sh {label}: --round wait|subvert is required — `wait` lands after an open round ends, `subvert` voids it; --dry-run shows the cost of each"));
            return USAGE;
        }
    }
    if a.evidence_tip != a.tip {
        w.err(format!("queue.sh {label}: --evidence-tip must equal --tip {} (got {:?}) — the checks must have run on the tip landed — refused", a.tip, a.evidence_tip));
        return FAIL;
    }
    if !simple::is_full_sha(a.tip) {
        w.err(format!("queue.sh {label}: --tip {}: not a full 40-hex commit name — refused", a.tip));
        return FAIL;
    }
    let Ok(c) = resolve(w, label, a.repo) else {
        return FAIL;
    };
    let Ok(path) = repo_path(w, label, &c) else {
        return FAIL;
    };
    if idents(w, label, &[("bead id", a.bead), ("repo", &c.r.name), ("by", a.by)]).is_err()
        || require_lc(w, label).is_err()
    {
        return FAIL;
    }
    let (Some(base), LandMode::QueueLocal) = (c.r.landref.clone(), c.r.mode) else {
        w.err(format!("queue.sh {label}: {} is not a queue.local repository — refused", c.r.name));
        return FAIL;
    };
    let Some(base_sha) = w.git.rev_parse(&path, &base) else {
        w.err(format!("queue.sh {label}: cannot resolve {base} — refused"));
        return FAIL;
    };
    if !w.git.commit_exists(&path, a.tip) {
        w.err(format!("queue.sh {label}: --tip {}: no such commit in {} — refused", a.tip, c.r.name));
        return FAIL;
    }
    let rounds = match open_rounds(w, &c, &path) {
        Ok(r) => r,
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot tell which rounds are open for {}: {e} — refused, nothing changed", c.r.name));
            return FAIL;
        }
    };
    let contains = w.git.is_ancestor(&path, &base_sha, a.tip);
    let (added, retired) = config_keys(w, &path, &base_sha, a.tip);
    for r in &rounds {
        w.out(format!(
            "queue.sh {label}: round {} phase {}, {} min in, members {}, {} green pass(es), cut on {}",
            r.batch, r.phase, r.minutes, ids(&r.members), r.greens, r.base
        ));
    }
    w.out(format!(
        "queue.sh {label}: tip {} {} {base} ({base_sha}); config keys the tip adds: {}; retires: {} (against {base})",
        a.tip,
        if contains { "contains" } else { "does NOT contain" },
        list(&added),
        list(&retired)
    ));
    w.out(format!("queue.sh {label}: {}", consequence(&rounds, a.round)));
    if a.dry_run {
        return OK;
    }
    let policy = a.round.unwrap_or(Policy::Subvert);
    if !contains {
        w.err(format!("queue.sh {label}: {} does not contain {base} ({base_sha}) — rebase it first; refused, nothing changed", a.tip));
        return FAIL;
    }
    let wait_bound = c.s.round_wall_secs + c.s.round_ami_wait_secs;
    let now = w.clock.now();
    let hold = spira_config::cut_hold::CutHold {
        owner: format!("hand-land {} by {}", a.bead, a.by),
        reason: one_line(&reason),
        pid: w.env.pid(),
        since: now,
        until: now + wait_bound + HOLD_MARGIN_SECS,
    };
    if let Err(e) = spira_config::cut_hold::place(&c.s.queue_dir, &c.r.name, &hold) {
        w.err(format!("queue.sh {label}: {e} — refused, nothing changed"));
        return FAIL;
    }
    let _hold = HoldGuard { c: &c, pid: hold.pid };
    let rounds = match open_rounds(w, &c, &path) {
        Ok(r) => r,
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot tell which rounds are open for {}: {e} — refused, nothing changed", c.r.name));
            return FAIL;
        }
    };

    let voided = match policy {
        Policy::Subvert => match subvert(w, &c, a, &reason, &rounds) {
            Ok(v) => v,
            Err(rc) => return rc,
        },
        Policy::Wait => {
            if let Err(rc) = wait_out(w, &c, &path, &rounds, wait_bound) {
                return rc;
            }
            "-".to_string()
        }
    };
    let head = w.git.rev_parse(&path, &base).unwrap_or_default();
    if !w.git.is_ancestor(&path, &head, a.tip) {
        w.err(format!("queue.sh {label}: {base} moved to {head} while this waited and {} does not contain it — rebase the tip and run again; nothing landed, the cut hold is lifted", a.tip));
        log(w, &c, a, policy, &voided, "refused: base moved");
        return FAIL;
    }
    land_and_verify(w, &c, &path, &base, a, &evidence, policy, &voided)
}

fn subvert(w: &World, c: &Ctx, a: &Args, reason: &str, rounds: &[OpenRound]) -> Result<String, i32> {
    let mut voided = Vec::new();
    for r in rounds {
        let before: Vec<(String, String)> = r
            .members
            .iter()
            .map(|m| (m.id.clone(), w.lc.bead_row(&m.id).map(|b| b.state).unwrap_or_default()))
            .collect();
        let token = match w.lc.round_lease(&c.r.name, &r.batch) {
            Ok(l) => l.map(|l| l.token),
            Err(e) => {
                w.err(format!("queue.sh hand-land: cannot tell whether round {} is leased: {e} — refused, nothing changed", r.batch));
                return Err(FAIL);
            }
        };
        let why = format!("cause=hand-landing by={} bead={} — {}", a.by, a.bead, one_line(reason));
        let rc = abandon_for_landing(w, &r.batch, &c.r.name, &why, token.as_deref());
        if rc != OK {
            w.err(format!("queue.sh hand-land: round {} could not be voided (rc={rc}) — refused, nothing landed", r.batch));
            return Err(FAIL);
        }
        for (id, was) in before {
            let now = w.lc.bead_row(&id).map(|b| b.state).unwrap_or_default();
            w.out(format!("queue.sh hand-land: member {id}: {was} in the round, returned as {now}"));
        }
        voided.push(r.batch.clone());
    }
    Ok(if voided.is_empty() { "-".into() } else { voided.join(",") })
}

fn wait_out(w: &World, c: &Ctx, path: &Path, rounds: &[OpenRound], bound: u64) -> Result<(), i32> {
    if rounds.is_empty() {
        return Ok(());
    }
    w.out(format!(
        "queue.sh hand-land: cut held; waiting for round {} to land or end — at most {bound}s (the round wall {}s plus the image wait {}s); a round that lands moves {}, and a tip that does not contain it is then refused",
        names(rounds), c.s.round_wall_secs, c.s.round_ami_wait_secs, c.r.landref.as_deref().unwrap_or("the landing ref")
    ));
    let deadline = w.clock.now() + bound;
    loop {
        match open_rounds(w, c, path) {
            Ok(open) if open.is_empty() => return Ok(()),
            Ok(open) if w.clock.now() >= deadline => {
                w.err(format!("queue.sh hand-land: round {} is still open after {bound}s — nothing landed, the cut hold is lifted; run again, or land with --round subvert", names(&open)));
                return Err(FAIL);
            }
            Ok(_) => {}
            Err(e) => {
                w.err(format!("queue.sh hand-land: cannot tell whether the round has ended: {e} — nothing landed"));
                return Err(FAIL);
            }
        }
        w.clock.sleep(POLL_SECS);
    }
}

#[allow(clippy::too_many_arguments)]
fn land_and_verify(
    w: &World,
    c: &Ctx,
    path: &Path,
    base: &str,
    a: &Args,
    evidence: &str,
    policy: Policy,
    voided: &str,
) -> i32 {
    let label = "hand-land";
    let state = w.lc.bead_row(a.bead).map(|b| b.state).unwrap_or_default();
    if matches!(state.as_str(), "SUBMITTED" | "CERTIFIED") {
        w.out(format!("queue.sh {label}: {} is {state}; no resubmit needed", a.bead));
    } else {
        let rc = simple::resubmit(
            w,
            a.bead,
            Some(&c.r.name),
            a.tip,
            a.by,
            a.reason,
            &Text::Arg(evidence.to_string()),
            a.evidence_tip,
        );
        if rc != OK {
            log(w, c, a, policy, voided, "refused: resubmit");
            return rc;
        }
    }
    if let Some(row) = w.lc.bead_row(a.bead).filter(|r| r.holds.iter().any(|h| h == "manual")) {
        if let Some((s, v)) = w.lc.bead_state(a.bead) {
            let kind = serde_json::json!({"Unhold": {"kind": "Manual"}}).to_string();
            if let Err((rc, e)) = w.lc.bead_event(a.bead, &s, &v, a.by, &kind) {
                w.err(format!("queue.sh {label}: could not lift {}'s manual hold (rc={rc}): {e} — nothing landed", row.bead_id));
                log(w, c, a, policy, voided, "refused: hold");
                return FAIL;
            }
        }
    }
    let previous = c
        .s
        .releases
        .as_deref()
        .map(super::deploy::current_name)
        .and_then(|n| Path::new(&n).file_name().map(|f| f.to_string_lossy().into_owned()))
        .filter(|n| n != "absent");
    let (rc, pending) = super::land::land_local_deferring(
        w,
        Some(&c.r.name),
        a.tip,
        &Text::Arg(format!("{}:{}", a.bead, a.tip)),
        a.worktree,
        false,
        true,
    );
    let landed = w.git.is_ancestor(path, a.tip, &w.git.rev_parse(path, base).unwrap_or_default());
    if rc != OK && !landed {
        if let Some(p) = pending {
            p.discard();
        }
        log(w, c, a, policy, voided, "refused: land-local");
        return rc;
    }
    let rollback = pending.as_ref().map(|p| p.rollback_plan());
    let outcome = pending.map(|p| p.activate(w));
    let verify = verify_release(w, a, &outcome, rollback.as_ref(), previous.as_deref());
    log(w, c, a, policy, voided, &verify.line);
    w.out(format!("queue.sh {label}: {} landed at {} onto {base}; verify: {}", a.bead, a.tip, verify.line));
    if verify.ok {
        return OK;
    }
    w.err(format!("queue.sh {label}: {base} stays at {} — revert the landing with: queue rollback-local {}", a.tip, c.r.name));
    DEPLOY_FAULT
}

struct Verify {
    ok: bool,
    line: String,
}

fn verify_release(
    w: &World,
    a: &Args,
    outcome: &Option<super::deploy::Outcome>,
    plan: Option<&super::deploy::Plan>,
    previous: Option<&str>,
) -> Verify {
    use super::deploy::Outcome;
    let failed = |why: String| {
        w.err(format!("queue.sh hand-land: {why}"));
        let undo = match (plan, previous) {
            (Some(p), Some(prev)) => super::deploy::verify(w, p, prev)
                .and_then(|()| super::deploy::activate(w, p, prev))
                .map(|()| format!("release {prev} re-activated"))
                .unwrap_or_else(|e| format!("rolling back to {prev} failed: {e}")),
            _ => "no previous release to roll back to".into(),
        };
        w.err(format!("queue.sh hand-land: {undo}"));
        Verify { ok: false, line: format!("failed: {} — {undo}", one_line(&why)) }
    };
    match outcome {
        None => Verify { ok: true, line: "skipped: this landing publishes no release".into() },
        Some(Outcome::Superseded(why)) => Verify { ok: true, line: format!("skipped: {why}") },
        Some(Outcome::Fault(why)) => failed(format!("activation failed: {why}")),
        Some(Outcome::Activated(sha)) => {
            for (what, args) in [
                ("lifecycle read", vec!["show", a.bead]),
                ("content read", vec!["content", "show", a.bead]),
            ] {
                if let Err(e) = w.lc.fresh_read(&args) {
                    return failed(format!("the {what} (`spira-lc {}`) through release {sha} was refused: {e}", args.join(" ")));
                }
            }
            Verify { ok: true, line: format!("ok: lifecycle and content read through release {sha}") }
        }
    }
}
