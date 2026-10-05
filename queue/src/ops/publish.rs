//! queue.local's publish queue (DESIGN.md §2.2, §8 D4).

use std::collections::BTreeMap;

use super::{czar_ok, Acquire, Ctx, idents, landing_log, lock, lock_held_by_caller, repo_path, resolve, take_lock, title_line, World, FAIL, OK};
use crate::model::{LandMode, Member};
use crate::ports::Divergence;
use crate::records::{self, write_atomic, Kv};

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
    match w.lib.divergence(&c.s.queue_dir, &name, &path, &forge_sha, &head_sha) {
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

    let members = match range_members(w, &path, &c.s.landstate, &forge_sha, &head_sha) {
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

/// The members of forge..head: from the land commits, landstate for tips only.
pub fn range_members(w: &World, path: &std::path::Path, landstate: &std::path::Path, forge: &str, head: &str) -> Result<Vec<Member>, String> {
    let commits = w.git.log_range(path, &format!("{forge}..{head}"))?;
    let states: BTreeMap<_, _> = records::all_land_states(landstate).unwrap_or_default().into_iter().collect();
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
}

/// The open publish PR as the pass saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pr {
    Open { ci: Ci, behind: bool },
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

/// None: no publish PR on record. A PR whose CI is not concluded, or concluded green, is
/// left to the verdict pass; only a concluded-red or untested head that local has moved
/// past is superseded (law-a-retry-must-change-an-input: the republish carries a new head).
pub fn decide(pr: Option<Pr>) -> Settle {
    match pr {
        None => Settle::Open,
        Some(Pr::Gone) => Settle::Retire,
        Some(Pr::Unknown) => Settle::Wait,
        Some(Pr::Open { ci: Ci::Red | Ci::Untested, behind: true }) => Settle::Supersede,
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
    let kv = records::read_kv(&pfile).ok().flatten();
    let pr = kv.as_ref().map(|kv| observe(w, c, &path, kv));
    let action = decide(pr);
    let now = w.clock.now();
    let pr_no = kv.as_ref().and_then(|k| k.get("pr")).unwrap_or("").to_string();
    w.out(format!("{now} publish-settle {name}: pr={} -> {action:?}", if pr_no.is_empty() { "none" } else { &pr_no }));
    match action {
        Settle::Wait => OK,
        Settle::Open => publish_with(w, Some(&name), true),
        Settle::Retire | Settle::Supersede => {
            if action == Settle::Supersede {
                w.forge.pr_close(&c.s.forge, &path, &pr_no);
            }
            let _ = std::fs::remove_file(&pfile);
            landing_log(&c.s.run, &format!("QUEUE PUBLISH_SUPERSEDED {now} repo={name} pr={pr_no} action={action:?}"));
            publish_with(w, Some(&name), true)
        }
    }
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
    Pr::Open { ci, behind: local != head }
}
