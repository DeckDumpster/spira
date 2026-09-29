//! queue.local's terminal step and its undo: land-local, rollback-local (DESIGN.md §2.2,
//! §8 D2/D3/D11).

use std::fs;
use std::path::{Path, PathBuf};

use super::{czar_ok, idents, lock_held_by_caller, read_text, repo_path, resolve, take_lock, World, FAIL, OK, USAGE};
use crate::cli::Text;
use crate::model::{LandMode, Member};
use crate::records::{self, write_atomic};

/// `$SPIRA_RELEASES/current` is a symlink: production runs an installed release, so a land
/// must package and activate one. Absent (or no releases dir configured): production runs a
/// checkout, nothing reads the symlink, and the release step is skipped (sp-zt0ae).
pub fn release_in_force(releases: Option<&Path>) -> bool {
    releases.map(|r| fs::symlink_metadata(r.join("current")).map(|m| m.file_type().is_symlink()).unwrap_or(false)).unwrap_or(false)
}

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
    let Some(base_sha) = w.git.rev_parse(&path, &base) else {
        w.err(format!("queue.sh land-local: cannot resolve {base}"));
        return FAIL;
    };

    // Row 4: alarm (never refuse) on a foreign forge divergence, against the CACHED
    // remote-tracking ref — no fetch on the round's critical path.
    if let Some((remote, branch)) = &c.r.publish {
        if let Some(fsha) = w.git.rev_parse(&path, &format!("refs/remotes/{remote}/{branch}")) {
            let _ = w.lib.divergence(&name, &path, &fsha, &base_sha);
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

    // What ships is the round's own build (§8 D2). Needed only when a release is in force
    // (§8 D3); checked whenever a worktree is named, so a wrong worktree never passes.
    let releases = c.s.releases.clone();
    let release_needed = release_in_force(releases.as_deref());
    let bins = match worktree {
        Some(wt) => match round_bins(w, &path, &head, wt) {
            Ok(b) => Some(b),
            Err(why) => {
                w.err(format!("queue.sh land-local: {why}; refused, nothing changed"));
                return FAIL;
            }
        },
        None if release_needed => {
            w.err(format!(
                "queue.sh land-local: --worktree <round worktree> is required while production runs a release ({}/current exists); refused, nothing changed",
                releases.as_deref().map(|p| p.display().to_string()).unwrap_or_default()
            ));
            return FAIL;
        }
        None => None,
    };

    // In checkout mode, when the landing repository is the running harness checkout, the
    // land also deploys it (§8 D11) — every precondition is checked here, before the CAS.
    let deploy_plan = match (release_needed, super::deploy::running_checkout(w, &c.s.home, &path)) {
        (false, Some(checkout)) => match super::deploy::prepare(w, &checkout, &base, &head, bins.as_deref()) {
            Ok(p) => Some(p),
            Err(why) => {
                w.err(format!("queue.sh land-local: {why}; refused, nothing changed"));
                return FAIL;
            }
        },
        _ => None,
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

    // Package and activate before anything else observes the land; a failure reverts.
    if release_needed {
        let rel = releases.clone().unwrap_or_default();
        match package_and_activate(w, &rel, &name, &head, &path, bins.as_deref().unwrap_or(Path::new(""))) {
            Ok(release) => w.out(format!("queue.sh land-local: activated {release}")),
            Err(why) => {
                if !why.is_empty() {
                    w.err(format!("queue.sh land-local: {why}"));
                }
                let _ = w.git.update_ref(&path, &base_ref, &base_sha, Some(&head));
                w.err(format!("queue.sh land-local: packaging/activation failed for {head} — reverted, nothing changed"));
                return FAIL;
            }
        }
    } else {
        w.err(format!(
            "queue.sh land-local: release step skipped: production runs a checkout (no {}/current)",
            releases.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| "<SPIRA_RELEASES unset>".into())
        ));
    }
    // The checkout deploy (§8 D11) runs right after the CAS; its failures are loud, never
    // revert the ref, and make the exit non-zero once the members are recorded.
    let deployed = match &deploy_plan {
        Some(plan) => super::deploy::run(w, plan, &head, &c.s.home, super::lifecycle_on(w)),
        None => true,
    };

    let seqfile = c.queue_file("round-seq");
    let n = records::read_seq(&seqfile) + 1;
    let archive = format!("refs/archive/rounds/{n}");
    let _ = w.git.update_ref(&path, &archive, &head, None);
    let _ = write_atomic(&seqfile, &format!("{n}\n"));

    let landed_reason = ungated.as_ref().map(|r| format!("ungated: {r}")).unwrap_or_default();
    for m in &ms {
        w.lib.land_mark(&m.id, "LANDED", &m.tip, &landed_reason);
        w.lib.gh_issue_closeout(&m.id, &head, &path);
        w.lib.bead_close_on_land(&m.id, &head);
        if ungated.is_some() {
            // bead_close_on_land re-marks LANDED with its own "Closed by landing pass"
            // reason; the ungated record must be the one that stays (§8 D12).
            w.lib.land_mark(&m.id, "LANDED", &m.tip, &landed_reason);
        }
        w.out(format!("queue.sh land-local: {} landed at {head}", m.id));
    }
    let listed = ms.iter().map(Member::render).collect::<Vec<_>>().join(",");
    w.lib.notify(
        &name,
        &format!("local landing (round {n})"),
        &format!(
            "{base} fast-forwarded to {head} (round {n}, archived at {archive}). Members: {listed}{}",
            ungated.as_ref().map(|r| format!(". UNGATED — no gate PASS or round GREEN for this tree; SPIRA_LAND_UNGATED={r}")).unwrap_or_default()
        ),
    );
    w.out(format!("queue.sh land-local: {base} fast-forwarded to {head} (round {n}, archived at {archive})"));
    if !deployed {
        w.err(format!("queue.sh land-local: {base} landed at {head} but the production checkout deploy did not complete — see LAND DEPLOY/SMOKE FAILED above"));
        return FAIL;
    }
    OK
}

/// build-tarball.sh with the round's own binaries, retained under `.tarballs` so a
/// rollback can re-activate it, then activate.sh (SPIRA_ACTIVATE_LAND_LOCAL=1).
fn package_and_activate(w: &World, releases: &Path, name: &str, head: &str, repo: &Path, bins: &Path) -> Result<String, String> {
    let retain = releases.join(".tarballs");
    fs::create_dir_all(&retain).map_err(|_| format!("cannot create {}", retain.display()))?;
    let release = format!("spira-{head}");
    let built = w
        .scripts
        .build_tarball(bins, name, &release, &retain, head, repo)
        .filter(|p| p.is_file())
        .ok_or_else(|| "build-tarball.sh did not produce a tarball".to_string())?;
    let (rc, out) = w.scripts.activate(&built, true);
    if !out.is_empty() {
        w.err(out.trim_end_matches('\n'));
    }
    if rc != 0 {
        return Err(String::new());
    }
    Ok(release)
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
    let releases = c.s.releases.clone().unwrap_or_default();
    let tarball = releases.join(".tarballs").join(format!("spira-{prev}.tar.gz"));
    if !tarball.is_file() {
        w.err(format!("queue.sh rollback-local: no retained tarball for the previous release at {}", tarball.display()));
        return FAIL;
    }
    let (rc, out) = w.scripts.activate(&tarball, false);
    if !out.is_empty() {
        w.err(out.trim_end_matches('\n'));
    }
    if rc != 0 {
        w.err(format!("queue.sh rollback-local: activate.sh failed to re-activate spira-{prev}"));
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
        &c.r.name,
        &format!("local rollback (round {n} -> {})", n - 1),
        &format!("{base} reset to {prev} (release spira-{prev} re-activated)."),
    );
    w.out(format!("queue.sh rollback-local: activated spira-{prev}, {base} reset to {prev}"));
    OK
}
