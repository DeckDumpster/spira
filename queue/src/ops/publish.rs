//! queue.local's publish queue (DESIGN.md §2.2, §8 D4).

use std::collections::BTreeMap;

use super::{czar_ok, Acquire, Ctx, idents, landing_log, lock, lock_held_by_caller, repo_path, resolve, take_lock, title_line, World, FAIL, OK};
use crate::model::{LandMode, Member};
use crate::ports::Divergence;
use crate::records::{self, write_atomic, Kv};
use spira_config::local_pass;

pub fn publish(w: &World, repo: Option<&str>) -> i32 {
    publish_with(w, repo, lock_held_by_caller(w))
}

/// `lock_held`: the caller (to-forge, or SPIRA_QUEUE_LOCK_HELD=1) already holds the lock.
pub fn publish_with(w: &World, repo: Option<&str>, lock_held: bool) -> i32 {
    if !czar_ok(w) {
        return FAIL;
    }
    let Ok(c) = resolve(w, "publish", repo) else { return FAIL };
    let Ok(path) = repo_path(w, "publish", &c) else { return FAIL };
    let name = c.r.name.clone();
    if c.r.mode != LandMode::QueueLocal {
        w.err(format!("queue.sh publish: repo is not in queue.local mode (mode={})", c.r.mode.as_str()));
        return FAIL;
    }
    let Some(base) = c.r.landref.clone() else {
        w.err(format!("queue.sh publish: cannot resolve landing ref for {name}"));
        return FAIL;
    };
    if c.r.ref_remote(&base).is_some() {
        w.err(format!("queue.sh publish: {name} resolves to a remote-tracking ref ({base}) — not a queue.local base"));
        return FAIL;
    }
    let Some((remote, forge_branch)) = c.r.publish.clone() else {
        w.err(format!("queue.sh publish: cannot resolve a forge target for {name}"));
        return FAIL;
    };
    if idents(w, "publish", &[("remote", &remote), ("forge branch", &forge_branch), ("base", &base)]).is_err() {
        return FAIL;
    }
    let _g = if lock_held {
        None
    } else {
        match take_lock(w, "publish", &c, "") {
            Ok(g) => g,
            Err(rc) => return rc,
        }
    };
    let pfile = c.queue_file("publish");
    if let Ok(Some(kv)) = records::read_kv(&pfile) {
        w.err(format!(
            "queue.sh publish: a publish PR is already open for {name} (pr={}) — settle it first",
            kv.get("pr").unwrap_or("")
        ));
        return FAIL;
    }
    if !w.git.fetch(&path, &remote, &forge_branch) {
        w.err(format!("queue.sh publish: could not fetch {remote}/{forge_branch}"));
        return FAIL;
    }
    let Some(forge_sha) = w.git.rev_parse(&path, &format!("refs/remotes/{remote}/{forge_branch}")) else {
        w.err(format!("queue.sh publish: cannot resolve {remote}/{forge_branch}"));
        return FAIL;
    };
    let Some(head_sha) = w.git.rev_parse(&path, &base) else {
        w.err(format!("queue.sh publish: cannot resolve {base}"));
        return FAIL;
    };
    match w.lib.divergence(&c.s.mailbox, &c.s.queue_dir, &name, &path, &forge_sha, &head_sha) {
        Divergence::Ancestor => {}
        Divergence::Diverged(foreign) => {
            w.err(format!(
                "queue.sh publish: {remote}/{forge_branch} is not an ancestor of {base} — refusing to publish (something else moved the forge; foreign range {foreign})"
            ));
            return FAIL;
        }
        Divergence::CannotCheck(why) => {
            w.err(format!("queue.sh publish: cannot check whether {remote}/{forge_branch} is an ancestor of {base} — refusing to publish: {why}"));
            return FAIL;
        }
    }
    if forge_sha == head_sha {
        let short: String = head_sha.chars().take(12).collect();
        w.out(format!("queue.sh publish: nothing to publish for {name} ({remote}/{forge_branch} already at {short})"));
        return OK;
    }

    // This exact head already went red (marker left by _verdict_settle_publish_red);
    // republishing it unchanged is a retry with no changed input (law-a-retry-must-change-an-input).
    let red_file = c.queue_file("publish-red");
    if let Ok(Some(kv)) = records::read_kv(&red_file) {
        if kv.get("head") == Some(head_sha.as_str()) {
            let fid = kv.get("fix_forward").unwrap_or("<unknown>");
            w.out(format!("queue.sh publish: waiting on fix-forward {fid} for {name} — {base} has not moved past the red head"));
            landing_log(&c.s.run, &format!("QUEUE PUBLISH_WAIT {} repo={name} fix_forward={fid} head={head_sha}", w.clock.now()));
            return OK;
        }
        let _ = std::fs::remove_file(&red_file);
    }

    let members = match range_members(w, &path, &forge_sha, &head_sha) {
        Ok(m) => m,
        Err(e) => {
            w.err(format!("queue.sh publish: cannot read {forge_sha}..{head_sha}: {e} — refusing"));
            return FAIL;
        }
    };
    if members.is_empty() {
        w.err(format!("queue.sh publish: {name} has new commits on {base} but no land commit (spira: land <id>) in range — refusing"));
        return FAIL;
    }

    let who = w.env.var("USER").unwrap_or_else(|| "unknown".into());
    let override_reason = w.env.var(local_pass::OVERRIDE_ENV);
    match local_pass::check(&c.s.run, local_pass::Kind::FullSuite, &head_sha, override_reason.as_deref(), &who, &w.clock.now().to_string()) {
        Ok(local_pass::Verdict::Passed) => {}
        Ok(local_pass::Verdict::Overridden(why)) => {
            w.out(format!("queue.sh publish: full-suite local-pass check for {head_sha} OVERRIDDEN by {who}: {why}"));
            landing_log(&c.s.run, &format!("QUEUE PUBLISH_OVERRIDE {} repo={name} head={head_sha} by={who}", w.clock.now()));
        }
        Err(why) => {
            w.err(format!("queue.sh publish: refusing to open a publish PR for {head_sha}: {why}"));
            return FAIL;
        }
    }

    let stamp = w.clock.stamp();
    let branch = format!("spira/publish/{stamp}");
    if !w.lib.push(&path, &remote, &format!("{head_sha}:refs/heads/{branch}")) {
        w.err(format!("queue.sh publish: could not push {branch}"));
        return FAIL;
    }
    let ids: Vec<String> = members.iter().map(|m| m.id.clone()).collect();
    let titles = w.bd.show(&ids).unwrap_or_default();
    let mut body = format!("Publish queue: {} bead(s) landed on {base} since the last publish, for {name}.\n\n", ids.len());
    body.push_str(&format!(
        "These commits already fast-forwarded {base} locally; CI here is confirmation, not the gate. A red run files a fix-forward bead — it never rolls back production.\n\n"
    ));
    for id in &ids {
        body.push_str(&format!("- {id} — {}\n", title_line(&titles, id)));
    }
    let title = format!("publish: {} bead(s) for {name}", ids.len());
    let Some(pr) = w.forge.pr_create(&c.s.forge, &path, &branch, &forge_branch, &title, body.trim_end_matches('\n')) else {
        w.err(format!("queue.sh publish: forge pr-create failed for {branch}"));
        return FAIL;
    };
    if pr.is_empty() {
        w.err(format!("queue.sh publish: forge returned no PR number for {branch}"));
        return FAIL;
    }
    let opened = w.clock.now();
    let mut rec = Kv::default();
    for (k, v) in [
        ("pr", pr.clone()),
        ("head", head_sha.clone()),
        ("base", forge_sha.clone()),
        ("members", crate::model::render_members(&members)),
        ("opened", opened.to_string()),
        ("branch", branch.clone()),
        ("remote", remote.clone()),
        ("forge_branch", forge_branch.clone()),
    ] {
        rec.push(k, &v);
    }
    if let Err(e) = write_atomic(&pfile, &rec.render()) {
        w.err(format!("queue.sh publish: {e}"));
        return FAIL;
    }
    landing_log(&c.s.run, &format!("QUEUE PUBLISH {opened} repo={name} pr={pr} members={}", ids.len()));
    w.out(format!("queue.sh publish: PR {pr} opened — {} bead(s) since last publish ({branch})", ids.len()));
    OK
}

/// The members of forge..head: from the land commits, the lifecycle rows for tips only.
pub fn range_members(w: &World, path: &std::path::Path, forge: &str, head: &str) -> Result<Vec<Member>, String> {
    let commits = w.git.log_range(path, &format!("{forge}..{head}"))?;
    let states: BTreeMap<_, _> = w.lc.bead_rows(Some("LANDED")).unwrap_or_default().into_iter().map(|r| (r.bead_id.clone(), r)).collect();
    let in_range = |t: &str| {
        crate::ident::check("tip", t).is_ok() && w.git.commit_exists(path, t) && w.git.is_ancestor(path, t, head) && !w.git.is_ancestor(path, t, forge)
    };
    Ok(crate::publish_range::members(&commits, &states, &in_range))
}

/// What a publish pass does about the repository's publish PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settle {
    Wait,
    Supersede,
    Retire,
    Open,
    /// The run's suites job was cancelled or timed out on the same head: run the whole workflow again.
    Rerun,
    /// The rerun was untested too: tell a human, once.
    Alarm,
    /// Already alarmed; the record waits for a hand.
    Parked,
}

/// The open publish PR as the pass saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pr {
    Open { ci: Ci, behind: bool, tries: u8 },
    Gone,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ci {
    Green,
    Red,
    /// The branch was never tested (a provision fault): its CI says nothing about the head.
    Untested,
    Pending,
}

pub fn ci_of(status: &str) -> Ci {
    match super::verdict::normalize_status(status.lines().next().unwrap_or("").trim()) {
        "green" => Ci::Green,
        "red" => Ci::Red,
        "harness_fault" => Ci::Untested,
        _ => Ci::Pending,
    }
}

/// `tries`: 0 never rerun, 1 rerun once, 2 and alarmed.
/// None: no publish PR on record. A PR whose CI is not concluded, or concluded green, is
/// left to the verdict pass; only a concluded-red or untested head that local has moved
/// past is superseded (law-a-retry-must-change-an-input: the republish carries a new head).
pub fn decide(pr: Option<Pr>) -> Settle {
    match pr {
        None => Settle::Open,
        Some(Pr::Gone) => Settle::Retire,
        Some(Pr::Unknown) => Settle::Wait,
        Some(Pr::Open { ci: Ci::Red | Ci::Untested, behind: true, .. }) => Settle::Supersede,
        Some(Pr::Open { ci: Ci::Untested, tries: 0, .. }) => Settle::Rerun,
        Some(Pr::Open { ci: Ci::Untested, tries: 1, .. }) => Settle::Alarm,
        Some(Pr::Open { ci: Ci::Untested, .. }) => Settle::Parked,
        Some(Pr::Open { .. }) => Settle::Wait,
    }
}

/// `queue publish-settle [<repo>]`: settle a stale publish PR, then publish; every
/// queue.local repository when none is named.
pub fn publish_settle(w: &World, repo: Option<&str>) -> i32 {
    let names = match repo {
        Some(r) => vec![r.to_string()],
        None => match w.lib.repos() {
            Ok(n) => n,
            Err(e) => {
                w.err(format!("queue.sh publish-settle: cannot list repositories: {e}"));
                return FAIL;
            }
        },
    };
    let mut rc = OK;
    for name in names {
        let Ok(c) = resolve(w, "publish-settle", Some(&name)) else {
            rc = FAIL;
            continue;
        };
        if c.r.mode != LandMode::QueueLocal {
            if repo.is_some() {
                w.err(format!("queue.sh publish-settle: {name} is not in queue.local mode (mode={})", c.r.mode.as_str()));
                rc = FAIL;
            }
            continue;
        }
        if settle_repo(w, &c) != OK {
            rc = FAIL;
        }
    }
    rc
}

fn settle_repo(w: &World, c: &Ctx) -> i32 {
    if !czar_ok(w) {
        return FAIL;
    }
    let name = c.r.name.clone();
    let Ok(path) = repo_path(w, "publish-settle", c) else { return FAIL };
    if idents(w, "publish-settle", &[("repo", &name)]).is_err() {
        return FAIL;
    }
    let _g = match lock::try_lock(&c.s.queue_dir, &name) {
        Acquire::Held(g) => g,
        Acquire::Busy => {
            w.out(format!("{} publish-settle {name}: another queue operation holds the lock; retry on the next tick", w.clock.now()));
            return OK;
        }
        Acquire::Unopenable => {
            w.err(format!("queue.sh publish-settle: cannot open lock file for {name}"));
            return FAIL;
        }
    };
    let pfile = c.queue_file("publish");
    let mut kv = records::read_kv(&pfile).ok().flatten();
    if kv.is_none() {
        match adopt_forge_publish(w, c, &path, &pfile) {
            Ok(false) => {}
            Ok(true) => {
                super::verdict::settle_publish(w, c, &path);
                kv = records::read_kv(&pfile).ok().flatten();
            }
            Err(()) => return FAIL,
        }
    }
    let pr = kv.as_ref().map(|kv| observe(w, c, &path, kv));
    let action = decide(pr);
    if !matches!(action, Settle::Wait | Settle::Parked | Settle::Alarm) && round_pass_running(w, c) {
        w.out(format!("{} publish-settle {name}: a round pass holds the hypervisor; deferring {action:?} to the next tick", w.clock.now()));
        return OK;
    }
    let now = w.clock.now();
    let pr_no = kv.as_ref().and_then(|k| k.get("pr")).unwrap_or("").to_string();
    w.out(format!("{now} publish-settle {name}: pr={} -> {action:?}", if pr_no.is_empty() { "none" } else { &pr_no }));
    match action {
        Settle::Wait | Settle::Parked => OK,
        Settle::Rerun | Settle::Alarm => rerun_or_alarm(w, c, &path, &pfile, kv.as_ref(), action),
        Settle::Open => publish_with(w, Some(&name), true),
        Settle::Retire | Settle::Supersede => {
            if action == Settle::Supersede {
                w.lib.cancel_runs(&c.s.forge, &path, kv.as_ref().and_then(|k| k.get("branch")).unwrap_or(""));
                w.forge.pr_close(&c.s.forge, &path, &pr_no);
            }
            let _ = std::fs::remove_file(&pfile);
            landing_log(&c.s.run, &format!("QUEUE PUBLISH_SUPERSEDED {now} repo={name} pr={pr_no} action={action:?}"));
            publish_with(w, Some(&name), true)
        }
    }
}

/// An open `spira/publish/*` PR the record does not know is recorded (the newest; any older
/// one is closed), so the one-PR guard's refusal becomes settlement. Ok(true): adopted.
fn adopt_forge_publish(w: &World, c: &Ctx, path: &std::path::Path, pfile: &std::path::Path) -> Result<bool, ()> {
    let name = &c.r.name;
    let Some(mut open) = w.forge.publish_prs(&c.s.forge, path) else {
        w.err(format!("queue.sh publish-settle: {name}: cannot list open publish PRs on the forge"));
        return Err(());
    };
    open.sort_by_key(|(n, _)| n.parse::<u64>().unwrap_or(0));
    let Some((pr, branch)) = open.pop() else { return Ok(false) };
    let Some((remote, forge_branch)) = c.r.publish.clone() else { return Ok(false) };
    let fail = |why: String| {
        w.err(format!("queue.sh publish-settle: {name}: open publish PR {pr} ({branch}) is not in the record and cannot be adopted: {why}"));
        w.lib.notify(&c.s.mailbox, name, &format!("publish PR {pr} cannot be adopted"), &format!("PR {pr} ({branch}) is open for {name} but publish-settle has no record of it and could not adopt it: {why}"));
        Err(())
    };
    if idents(w, "publish-settle", &[("remote", &remote), ("forge branch", &forge_branch), ("branch", &branch)]).is_err() {
        return Err(());
    }
    if !w.git.fetch(path, &remote, &branch) || !w.git.fetch(path, &remote, &forge_branch) {
        return fail(format!("could not fetch {remote}/{branch}"));
    }
    let (Some(head), Some(base)) = (
        w.git.rev_parse(path, &format!("refs/remotes/{remote}/{branch}")),
        w.git.rev_parse(path, &format!("refs/remotes/{remote}/{forge_branch}")),
    ) else {
        return fail(format!("cannot resolve {remote}/{branch}"));
    };
    let members = range_members(w, path, &base, &head).unwrap_or_default();
    let mut rec = Kv::default();
    for (k, v) in [
        ("pr", pr.clone()),
        ("head", head),
        ("base", base),
        ("members", crate::model::render_members(&members)),
        ("opened", w.clock.now().to_string()),
        ("branch", branch.clone()),
        ("remote", remote),
        ("forge_branch", forge_branch),
    ] {
        rec.push(k, &v);
    }
    if let Err(e) = write_atomic(pfile, &rec.render()) {
        w.err(format!("queue.sh publish-settle: {e}"));
        return Err(());
    }
    for (n, _) in &open {
        w.forge.pr_close(&c.s.forge, path, n);
    }
    landing_log(&c.s.run, &format!("QUEUE PUBLISH_ADOPTED {} repo={name} pr={pr} branch={branch} closed={}", w.clock.now(), open.len()));
    w.out(format!("publish-settle {name}: adopted open publish PR {pr} ({branch}) from the forge"));
    Ok(true)
}

fn rerun_or_alarm(w: &World, c: &Ctx, path: &std::path::Path, pfile: &std::path::Path, kv: Option<&Kv>, action: Settle) -> i32 {
    let name = &c.r.name;
    let Some(mut rec) = kv.cloned() else { return OK };
    let pr = rec.get("pr").unwrap_or("").to_string();
    let branch = rec.get("branch").unwrap_or("").to_string();
    if action == Settle::Rerun {
        let Some(run) = w.forge.run_id(&c.s.forge, path, &branch) else {
            w.err(format!("queue.sh publish-settle: {name}: no run id for {branch}; cannot rerun PR {pr}"));
            return FAIL;
        };
        w.forge.workflow_rerun(&c.s.forge, path, &run);
        rec.remove("rerun");
        rec.push("rerun", &run);
        landing_log(&c.s.run, &format!("QUEUE PUBLISH_RERUN {} repo={name} pr={pr} run={run}", w.clock.now()));
        w.out(format!("publish-settle {name}: PR {pr} run {run} was never tested (suites cancelled or timed out) — whole workflow rerun"));
    } else {
        rec.remove("alarmed");
        rec.push("alarmed", &w.clock.now().to_string());
        landing_log(&c.s.run, &format!("QUEUE PUBLISH_UNTESTED {} repo={name} pr={pr}", w.clock.now()));
        w.lib.notify(
            &c.s.mailbox,
            name,
            &format!("publish PR {pr} untested after a rerun"),
            &format!("PR {pr} for {name} concluded without testing its head twice running (suites job cancelled or timed out). It will not be rerun again; look at the run and rerun or close it by hand."),
        );
    }
    match write_atomic(pfile, &rec.render()) {
        Ok(()) => OK,
        Err(e) => {
            w.err(format!("queue.sh publish-settle: {e}"));
            FAIL
        }
    }
}

const LIVE_PHASES: [&str; 5] = ["vm", "build", "unit-build", "suites", "unit-tests"];
const PROGRESS_FRESH_SECS: u64 = 600;

/// A publish PR's CI provisions VMs beside the round VM and starves its pass, so the cut waits.
/// A pass counts as running when its progress file is in a live phase and fresh, or its batch is
/// CI_RUNNING; an unreadable round record or batch state counts as running.
fn round_pass_running(w: &World, c: &Ctx) -> bool {
    if progress_live(&c.s.run, w.clock.now()) {
        return true;
    }
    crate::ops::round::open_records(c)
        .iter()
        .filter_map(|(_, kv)| kv.get("batch_id"))
        .any(|batch| w.lc.batch_state(batch).map_or(true, |(s, _)| s == "CI_RUNNING"))
}

fn progress_live(run: &std::path::Path, now: u64) -> bool {
    let Some(v) = std::fs::read_to_string(run.join("round-progress.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else { return false };
    let live = v["phase"].as_str().is_some_and(|p| LIVE_PHASES.contains(&p));
    let at = v["updated_at"].as_u64().unwrap_or(0);
    live && now.saturating_sub(at) < PROGRESS_FRESH_SECS
}

fn observe(w: &World, c: &Ctx, path: &std::path::Path, kv: &Kv) -> Pr {
    let field = |k: &str| kv.get(k).unwrap_or("").to_string();
    let (pr, branch, head) = (field("pr"), field("branch"), field("head"));
    if pr.is_empty() || branch.is_empty() || head.is_empty() {
        return Pr::Unknown;
    }
    match w.forge.pr_state(&c.s.forge, path, &pr).as_deref().map(str::trim) {
        Some("open") => {}
        Some("closed" | "merged") => return Pr::Gone,
        _ => return Pr::Unknown,
    }
    let Some(base) = c.r.landref.as_deref() else { return Pr::Unknown };
    let Some(local) = w.git.rev_parse(path, base) else { return Pr::Unknown };
    let ci = ci_of(&w.forge.check_status(&c.s.forge, path, &pr, &branch).unwrap_or_default());
    let tries = if kv.get("alarmed").is_some() { 2 } else if kv.get("rerun").is_some() { 1 } else { 0 };
    Pr::Open { ci, behind: local != head, tries }
}
