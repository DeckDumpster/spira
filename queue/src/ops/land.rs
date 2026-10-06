//! queue.local's terminal step and its undo: land-local, rollback-local (DESIGN.md §2.2,
//! §8 D2/D12/D13).

use std::fs;
use std::path::{Path, PathBuf};

use super::{actor, czar_ok, idents, require_lc, lock_held_by_caller, read_text, repo_path, resolve, take_lock, World, FAIL, OK, USAGE};
use crate::cli::Text;
use crate::model::{LandMode, Member};
use crate::records::{self, write_atomic};

/// The round's own binaries: `<worktree>/target/release`, when that worktree is checked
/// out at `head`'s tree and holds at least one executable (testenv's SPIRA_ARTIFACTS
/// contract, testenv/DESIGN.md §5). Err is the refusal text.
pub fn round_bins(w: &World, repo: &Path, head: &str, worktree: &Path) -> Result<PathBuf, String> {
    let want = w.git.rev_parse(repo, &format!("{head}^{{tree}}"));
    let have = w.git.rev_parse(worktree, "HEAD^{tree}");
    if want.is_none() || want != have {
        return Err(format!(
            "the worktree at {} is not at {head}'s tree (HEAD^{{tree}}={}) — its target/release is not this round's build",
            worktree.display(),
            have.unwrap_or_else(|| "<none>".into())
        ));
    }
    let dir = worktree.join("target").join("release");
    let has_exe = fs::read_dir(&dir)
        .map(|rd| rd.flatten().any(|e| super::simple::is_executable(&e.path())))
        .unwrap_or(false);
    if !has_exe {
        return Err(format!(
            "no built binaries for {head} at {} — run testenv --profile release in the round worktree first",
            dir.display()
        ));
    }
    Ok(dir)
}

/// §8 D12: Ok when a gate PASS or a round GREEN certifies exactly `head`'s tree in `repo`
/// (`gate::cert`, matched by (repo, tree) alone), naming it on stderr; Err is the refusal
/// text, which names the tree and the gate command that would certify it.
pub fn certified(w: &World, path: &Path, repo: &str, s: &crate::ports::Settings, head: &str, head_arg: &str) -> Result<(), String> {
    let Some(tree) = w.git.rev_parse(path, &format!("{head}^{{tree}}")) else {
        return Err(format!("cannot resolve {head}^{{tree}} — nothing can say what would land"));
    };
    let verdicts = gate::cert::verdicts_dir(w.var("SPIRA_VERDICTS").as_deref(), &s.run);
    let found = gate::cert::path(&verdicts, repo, &tree)
        .and_then(|p| w.env.read_file(&p).ok())
        .and_then(|text| gate::cert::certifies(&text, repo, &tree));
    match found {
        Some(c) => {
            w.err(format!(
                "queue.sh land-local: tree {tree} certified by {} {} ({}, {} at {})",
                c.source.word(),
                c.source.verdict(),
                c.rev,
                c.branch,
                c.when
            ));
            Ok(())
        }
        None => Err(format!(
            "no gate PASS or round GREEN for {head}'s tree {tree} in {repo} — nothing certified what would land; run: bash {}/gate.sh {head_arg} {repo}, then retry (or SPIRA_LAND_UNGATED=<reason> to land it ungated, logged)",
            s.home.display()
        )),
    }
}

/// Members from `--members`: `id:tip` tokens, comma or space separated; a token with no
/// tip (or an empty one) landed at the head.
pub fn land_members(text: &str, head: &str) -> Vec<Member> {
    crate::model::parse_members(text)
        .into_iter()
        .map(|m| if m.tip.is_empty() { Member { id: m.id, tip: head.to_string() } } else { m })
        .collect()
}

pub fn land_local(w: &World, repo: Option<&str>, head_arg: &str, members: &Text, worktree: Option<&Path>) -> i32 {
    if !czar_ok(w) {
        return FAIL;
    }
    let members_text = match read_text(w, members) {
        Ok(t) => t,
        Err(e) => {
            w.err(format!("queue.sh land-local: cannot read the members: {e}"));
            return USAGE;
        }
    };
    if members_text.trim().is_empty() {
        w.err("queue.sh land-local: --members is required");
        return USAGE;
    }
    let Ok(c) = resolve(w, "land-local", repo) else { return FAIL };
    let Ok(path) = repo_path(w, "land-local", &c) else { return FAIL };
    let name = c.r.name.clone();
    if c.r.mode != LandMode::QueueLocal {
        w.err(format!("queue.sh land-local: repo is not in queue.local mode (mode={})", c.r.mode.as_str()));
        return FAIL;
    }
    let Some(base) = c.r.landref.clone() else {
        w.err(format!("queue.sh land-local: cannot resolve landing ref for {name}"));
        return FAIL;
    };
    // queue.local's base is a LOCAL branch that happens to contain a slash (local/main).
    if c.r.ref_remote(&base).is_some() {
        w.err(format!("queue.sh land-local: {name} resolves to a remote-tracking ref ({base}) — not a queue.local base"));
        return FAIL;
    }
    if idents(w, "land-local", &[("head", head_arg), ("base", &base)]).is_err() {
        return FAIL;
    }
    let Some(head) = w.git.rev_parse(&path, head_arg) else {
        w.err("queue.sh land-local: cannot resolve head");
        return FAIL;
    };
    if let Some(wt) = worktree {
        if let Err(e) = crate::ident::check_path("worktree", &wt.display().to_string()) {
            w.err(format!("queue.sh land-local: {e} — refused"));
            return FAIL;
        }
    }
    let ms = land_members(&members_text, &head);
    for m in &ms {
        if idents(w, "land-local", &[("member id", &m.id), ("member tip", &m.tip)]).is_err() {
            return FAIL;
        }
    }
    let _g = if lock_held_by_caller(w) {
        None
    } else {
        match take_lock(w, "land-local", &c, "") {
            Ok(g) => g,
            Err(rc) => return rc,
        }
    };
    if require_lc(w, "land-local").is_err() {
        return FAIL;
    }
    let Some(base_sha) = w.git.rev_parse(&path, &base) else {
        w.err(format!("queue.sh land-local: cannot resolve {base}"));
        return FAIL;
    };

    // Row 4: alarm (never refuse) on a foreign forge divergence, against the CACHED
    // remote-tracking ref — no fetch on the round's critical path.
    if let Some((remote, branch)) = &c.r.publish {
        if let Some(fsha) = w.git.rev_parse(&path, &format!("refs/remotes/{remote}/{branch}")) {
            let _ = w.lib.divergence(&c.s.mailbox, &c.s.queue_dir, &name, &path, &fsha, &base_sha);
        }
    }

    if !w.git.is_ancestor(&path, &base_sha, &head) {
        w.err(format!("queue.sh land-local: {head} does not fast-forward from {base} ({base_sha}) — refused, nothing changed"));
        return FAIL;
    }

    // Only a certified tree lands (§8 D12): a gate PASS or a round GREEN for exactly this
    // head's tree, or the named, logged override.
    let ungated = match certified(w, &path, &name, &c.s, &head, head_arg) {
        Ok(()) => None,
        Err(refusal) => match w.var("SPIRA_LAND_UNGATED").map(|r| crate::ident::bounded_text(&r)).filter(|r| !r.is_empty()) {
            Some(reason) => Some(reason),
            None => {
                w.err(format!("queue.sh land-local: {refusal}; refused, nothing changed"));
                return FAIL;
            }
        },
    };

    // A landing of the harness repository publishes a release (§8 D13); every precondition
    // is checked here, before the CAS. What ships is the round's own tested build (§8 D2).
    let pin = worktree.map(|wt| wt.join("target").join("release").join(gate::target::PIN));
    let bins = match worktree {
        Some(wt) => match round_bins(w, &path, &head, wt) {
            Ok(b) => Some(b),
            Err(why) => {
                w.err(format!("queue.sh land-local: {why}; refused, nothing changed"));
                return FAIL;
            }
        },
        None => None,
    };
    // Only a landing of the harness (the repository releases are made from), and only
    // while a release is in force: before the cutover nothing runs one, and the first
    // activation is the cutover's, never a routine landing's.
    let in_force = c.s.releases.clone().filter(|r| super::deploy::release_in_force(r));
    let plan = if name != c.s.home_repo {
        None
    } else if let Some(releases) = in_force {
        match deploy_plan(&c.s, &path, &base, releases, bins) {
            Ok(p) => Some(p),
            Err(why) => {
                w.err(format!("queue.sh land-local: {why}; refused, nothing changed"));
                return FAIL;
            }
        }
    } else {
        w.err(format!(
            "queue.sh land-local: release step skipped: no release is in force ({}/current is absent) — production does not run {head} until a release is activated",
            c.s.releases.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| "<SPIRA_RELEASES unset>".into())
        ));
        None
    };

    // A CAS, never a plain write.
    let base_ref = format!("refs/heads/{base}");
    if !w.git.update_ref(&path, &base_ref, &head, Some(&base_sha)) {
        w.err(format!("queue.sh land-local: {base} moved concurrently — refused, nothing changed"));
        return FAIL;
    }
    if let Some(reason) = &ungated {
        let tree = w.git.rev_parse(&path, &format!("{head}^{{tree}}")).unwrap_or_else(|| "-".into());
        w.err(format!(
            "queue.sh land-local: UNGATED LANDING of {head} (tree {tree}) onto {base} — no gate PASS or round GREEN certified it; SPIRA_LAND_UNGATED={reason}"
        ));
        super::landing_log(&c.s.run, &format!("QUEUE UNGATED {} repo={name} head={head} tree={tree} reason={reason}", w.clock.now()));
    }

    let seqfile = c.queue_file("round-seq");
    let n = records::read_seq(&seqfile) + 1;
    let archive = format!("refs/archive/rounds/{n}");
    let _ = w.git.update_ref(&path, &archive, &head, None);
    let _ = write_atomic(&seqfile, &format!("{n}\n"));

    for m in &ms {
        w.lib.gh_issue_closeout(&m.id, &head, &path);
        super::helpers::close_on_land(w, &c.s.submitted_label, &m.id, &head);
        w.out(format!("queue.sh land-local: {} landed at {head}", m.id));
    }
    let lc_faults = ms.iter().filter(|m| !lc_deliver(w, &path, &head, m)).count();
    // The landing is recorded; now publish its release (§8 D13). A fault never reverts the
    // ref or the records: it leaves `current` where it was and makes the exit non-zero.
    let outcome = plan.as_ref().map(|p| super::deploy::run(w, p, &head));
    if let Some(pin) = &pin {
        let _ = fs::remove_file(pin);
    }
    let fault_marker = c.queue_file("deploy-fault");
    let deploy_note = match (&outcome, &plan) {
        (Some(super::deploy::Outcome::Activated(sha)), _) => {
            let _ = fs::remove_file(&fault_marker);
            w.out(format!("queue.sh land-local: activated release {sha}"));
            format!(" Release {sha} activated.")
        }
        (Some(super::deploy::Outcome::Fault(why)), Some(p)) => {
            w.err(format!(
                "LAND DEPLOY FAILED for {head}: {why} — current is untouched (still {}); {base} is at {head} and the landing stays recorded",
                super::deploy::current_name(&p.releases)
            ));
            let _ = write_atomic(&fault_marker, &format!("{head} {why}\n"));
            format!(" DEPLOY FAULT: {head} landed but its release was not activated: {why}.")
        }
        _ => String::new(),
    };
    let listed = ms.iter().map(Member::render).collect::<Vec<_>>().join(",");
    w.lib.notify(
        &c.s.mailbox,
        &name,
        &format!("local landing (round {n})"),
        &format!(
            "{base} fast-forwarded to {head} (round {n}, archived at {archive}). Members: {listed}{}{deploy_note}",
            ungated.as_ref().map(|r| format!(". UNGATED — no gate PASS or round GREEN for this tree; SPIRA_LAND_UNGATED={r}")).unwrap_or_default()
        ),
    );
    w.out(format!("queue.sh land-local: {base} fast-forwarded to {head} (round {n}, archived at {archive})"));
    if matches!(outcome, Some(super::deploy::Outcome::Fault(_))) {
        return super::DEPLOY_FAULT;
    }
    if lc_faults > 0 {
        return FAIL;
    }
    OK
}

/// A landed member's walk on spira-lc: Deliver (CERTIFIED -> IN_DELIVERY) then Delivered
/// (-> LANDED), from the row's own state and version. The landing is already recorded, so a
/// refusal is reported, never undone; false when the member did not reach LANDED.
pub fn lc_deliver(w: &World, path: &Path, head: &str, m: &Member) -> bool {
    let fail = |why: String| {
        w.err(format!("queue.sh land-local: spira-lc: {}: {why} — landed on the ref but not LANDED on spira-lc", m.id));
        false
    };
    if !w.git.is_ancestor(path, &m.tip, head) {
        return fail(format!("tip {} is not an ancestor of {head}", m.tip));
    }
    let Some((mut state, version)) = w.lc.bead_state(&m.id) else {
        return fail("no lifecycle row".into());
    };
    let Ok(mut v) = version.trim().parse::<u64>() else {
        return fail(format!("unreadable version {version:?}"));
    };
    let who = actor(w);
    if state == "CERTIFIED" {
        if let Err((rc, e)) = w.lc.bead_event(&m.id, &state, &v.to_string(), &who, "\"Deliver\"") {
            return fail(format!("Deliver refused (rc={rc}): {e}"));
        }
        state = "IN_DELIVERY".into();
        v += 1;
    }
    if state != "IN_DELIVERY" {
        return fail(format!("in state {state}, not CERTIFIED or IN_DELIVERY"));
    }
    let kind = format!("{{\"Delivered\":{{\"merge_sha\":\"{}\",\"proof\":\"ancestry\"}}}}", m.tip);
    match w.lc.bead_event(&m.id, &state, &v.to_string(), &who, &kind) {
        Ok(()) => true,
        Err((rc, e)) => fail(format!("Delivered refused (rc={rc}): {e}")),
    }
}

/// §8 D13's precondition for a landing of the harness while a release is in force: the
/// round's tested build to publish (law-deploy-the-tested-artifacts — never a rebuild). Err
/// is the refusal reason.
fn deploy_plan(s: &crate::ports::Settings, repo: &Path, landref: &str, releases: PathBuf, bins: Option<PathBuf>) -> Result<super::deploy::Plan, String> {
    let Some(bins) = bins else {
        return Err(format!(
            "--worktree <round worktree> is required to land {}: its release ships the round's own tested build (law-deploy-the-tested-artifacts), never a rebuild",
            s.home_repo
        ));
    };
    Ok(super::deploy::Plan { repo: repo.to_path_buf(), landref: landref.to_string(), releases, run: s.run.clone(), bins: Some(bins), db: s.db.clone() })
}

pub fn rollback_local(w: &World, repo: Option<&str>) -> i32 {
    if !czar_ok(w) {
        return FAIL;
    }
    let Ok(c) = resolve(w, "rollback-local", repo) else { return FAIL };
    let Ok(path) = repo_path(w, "rollback-local", &c) else { return FAIL };
    if c.r.mode != LandMode::QueueLocal {
        w.err(format!("queue.sh rollback-local: repo is not in queue.local mode (mode={})", c.r.mode.as_str()));
        return FAIL;
    }
    let Some(base) = c.r.landref.clone() else {
        w.err(format!("queue.sh rollback-local: cannot resolve landing ref for {}", c.r.name));
        return FAIL;
    };
    let _g = if lock_held_by_caller(w) {
        None
    } else {
        match take_lock(w, "rollback-local", &c, "") {
            Ok(g) => g,
            Err(rc) => return rc,
        }
    };
    let n = records::read_seq(&c.queue_file("round-seq"));
    if n < 2 {
        w.err(format!("queue.sh rollback-local: round-seq is {n} — no landed round to roll back to"));
        return FAIL;
    }
    let Some(prev) = w.git.rev_parse(&path, &format!("refs/archive/rounds/{}", n - 1)) else {
        w.err(format!("queue.sh rollback-local: refs/archive/rounds/{} has no archived head", n - 1));
        return FAIL;
    };
    // Re-activate the previous round's release (§8 D13): it was published when that round
    // landed, so it is verified and activated as it stands — never rebuilt.
    let Some(releases) = c.s.releases.clone() else {
        w.err("queue.sh rollback-local: SPIRA_RELEASES does not resolve — no release to roll back to");
        return FAIL;
    };
    if !super::deploy::release_in_force(&releases) {
        w.err(format!("queue.sh rollback-local: no release is in force ({}/current is absent) — nothing to roll back", releases.display()));
        return FAIL;
    }
    if !releases.join(&prev).is_dir() {
        w.err(format!("queue.sh rollback-local: no release {prev} in {} (pruned, or that round never published one)", releases.display()));
        return FAIL;
    }
    let plan = super::deploy::Plan { repo: path.clone(), landref: base.clone(), releases, run: c.s.run.clone(), bins: None, db: c.s.db.clone() };
    if let Err(why) = super::deploy::verify(w, &plan, &prev).and_then(|()| super::deploy::activate(w, &plan, &prev)) {
        w.err(format!("queue.sh rollback-local: {why} — could not re-activate release {prev}; nothing changed"));
        return FAIL;
    }
    let base_ref = format!("refs/heads/{base}");
    match w.git.rev_parse(&path, &base) {
        Some(cur) => {
            if !w.git.update_ref(&path, &base_ref, &prev, Some(&cur)) {
                w.err(format!("queue.sh rollback-local: {base} moved concurrently — release rolled back but the ref did not"));
                return FAIL;
            }
        }
        None => {
            let _ = w.git.update_ref(&path, &base_ref, &prev, None);
        }
    }
    w.lib.notify(
        &c.s.mailbox,
        &c.r.name,
        &format!("local rollback (round {n} -> {})", n - 1),
        &format!("{base} reset to {prev} (release {prev} re-activated)."),
    );
    w.out(format!("queue.sh rollback-local: activated release {prev}, {base} reset to {prev}"));
    OK
}
