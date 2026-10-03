//! The destruction chokepoint (wave 4.20, sp-9envm): every removal of a bead's worktree or
//! branch goes through this module, and nothing outside it may call `git worktree remove`,
//! `git branch -D` or `rm -rf` on a tree. Ported from lib.sh's DESTRUCTION section
//! (`spira_destroy_worktree`, `spira_destroy_branch`, `spira_reap_landed_branch`,
//! `spira_prune_worktrees`, `salvage`, `hold_alive`, `holder_alive`,
//! `spira_holder_witnesses`, `worktree_of`, `spira_caller`, `spira_reaplog`), which become
//! one-line shims in lib.sh calling this crate's `sending` binary (`destroy-worktree`,
//! `destroy-branch`, `prune`, `salvage`, plus the smaller plumbing verbs the shims need).
//!
//! WHY IT IS ONE SITE, and what it unconditionally refuses: see lib.sh's own DESTRUCTION
//! header (preserved there) — it is reproduced in full in `sending/DESIGN.md` §5 and is not
//! repeated here. In short: only paths under `$SPIRA_RUN/worktree/` may be removed; two
//! liveness witnesses are required (a live process, and the bead's own status, the latter
//! proved reachable before its absence counts); salvage runs first and its failure aborts
//! the removal; the content fence asks `content_on_base`, never a tip comparison; and every
//! decision is logged to the reap log, naming the bead and the calling program.
//!
//! lib.sh functions this module does NOT absorb, because they belong to families that have
//! not moved to Rust yet and stay exactly as they are: `spira_landref`/`ref_remote` (family
//! W — base refs). The claim witness is the lifecycle row's, reached through this module's
//! `ClaimProbe` trait (one implementor per crate, wired to `spira-lc`; sp-mve9i retired the
//! bd status witness), and the base through an explicit `base`/`remote` parameter the caller
//! resolves however it already does.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::git::Git;

/// `date -u +%Y-%m-%dT%H:%M:%SZ`, duplicated from real.rs's own copy because both need it
/// with no clock dependency injected (lib.sh's `date -u` is likewise a bare call).
pub fn utc_now() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!("{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// lib.sh `spira_caller`: the program that reached the chokepoint, read from `/proc` rather
/// than matched against a command line (a pattern matches the searcher's own argv, which is
/// how `pgrep -f` once reported a collector healthy by finding the shell that was killing
/// it). Walks the ancestor chain up to 6 levels; the interesting name is rarely the
/// immediate one (`sending.sh` tells you nothing, `test-repo.sh -> sending.sh` tells you
/// everything).
pub fn spira_caller() -> String {
    let mut out: Vec<String> = Vec::new();
    let mut pid = std::process::id();
    for _ in 0..6 {
        if pid == 1 {
            break;
        }
        let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else { break };
        let cmd: Vec<String> = raw.split(|&b| b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect();
        let first = cmd
            .iter()
            .filter(|t| !t.starts_with('-'))
            .take(3)
            .find(|t| t.ends_with(".sh") || t.ends_with(".py"))
            .map(|t| Path::new(t).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| t.clone()));
        if let Some(name) = first {
            out.push(name);
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { break };
        let Some(ppid) = stat.rsplit(')').next().unwrap_or("").split_whitespace().nth(1).and_then(|s| s.parse::<u32>().ok()) else { break };
        pid = ppid;
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        // Names were collected innermost-first; the shell built the chain outermost-first
        // ("a -> b -> sending.seam.sh"), so reverse.
        out.reverse();
        out.join(" -> ")
    }
}

/// lib.sh `spira_reaplog <verb> <id> <detail>`: one line, appended, never fatal.
pub fn reaplog(path: &Path, verb: &str, id: &str, detail: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let line = format!("{} {:<9} {:<22} {} [by {}]\n", utc_now(), verb, id, detail, spira_caller());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// lib.sh `worktree_of <branch> [repo]`: the registered worktree path holding it.
pub fn worktree_of(repo: &Path, br: &str) -> Option<PathBuf> {
    Git(repo).worktree_of(br)
}

fn pid_alive(pid: &str) -> bool {
    let pid = pid.trim();
    !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()) && Path::new(&format!("/proc/{pid}")).is_dir()
}

/// lib.sh `hold_alive <pidfile>`: 0 if the recorded pid is a live process. Unlike
/// `aeon_alive` this does NOT check argv, because the holder can be any process — a brain
/// session, the concierge, a hand-run tool.
pub fn hold_alive(pf: &Path) -> bool {
    let Ok(pid) = std::fs::read_to_string(pf) else { return false };
    pid_alive(&pid)
}

/// The rule lib.sh `aeon_alive`, strand and landing-pass each check: a space-joined cmdline
/// is an aeon when its argv[0] is `aeon`/`…/aeon` (the Rust binary) or it mentions the
/// retired `aeon.sh`.
fn is_aeon_cmdline(cmd: &str) -> bool {
    let argv0 = cmd.split(' ').next().unwrap_or("");
    cmd.contains("aeon.sh") || argv0 == "aeon" || argv0.ends_with("/aeon")
}

fn aeon_alive(pf: &Path) -> bool {
    let Ok(pid) = std::fs::read_to_string(pf) else { return false };
    let pid = pid.trim();
    if !pid_alive(pid) {
        return false;
    }
    let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    is_aeon_cmdline(&String::from_utf8_lossy(&cmd).replace('\0', " "))
}

/// lib.sh `holder_alive <id>`: a live process is working this bead — a hold pidfile (pid
/// only, the holder can be anything) OR a live aeon pidfile (pid AND argv, so a recycled pid
/// cannot resurrect a dead aeon's claim). Both satisfy the SAME predicate the reaper reads.
pub fn holder_alive(run: &Path, id: &str) -> bool {
    if hold_alive(&run.join(format!("hold-{id}.pid"))) {
        return true;
    }
    let Ok(rd) = std::fs::read_dir(run) else { return false };
    let suffix = format!("-{id}.pid");
    rd.flatten().any(|e| {
        let n = e.file_name().to_string_lossy().into_owned();
        n.starts_with("aeon-") && n.ends_with(&suffix) && aeon_alive(&e.path())
    })
}

/// What this module still cannot answer itself: who holds the bead's claim, which is the
/// lifecycle row's (design §3.4: bd status is inert for work beads — sp-mve9i retired the bd
/// `in_progress` witness this probe used to carry). `Ok(None)`: the machine answered and has
/// no row; `Err`: it did not answer.
pub trait ClaimProbe {
    fn probe(&self, id: &str) -> Result<Option<spira_config::lc_state::Row>, String>;
}

fn is_queued_state(state: &str) -> bool {
    matches!(state, "CERTIFIED" | "IN_DELIVERY")
}

/// `spira-lc state <id>` by bare name (an actual binary, not a bash function): `Some(state)`
/// on exit 0, `None` otherwise (lifecycle off, no binary, DB down, not yet classified —
/// every one of those must be able to make this call LESS restrictive, never more, so a
/// failure here falls through to bd's own signal, exactly as lib.sh's `|| true` chain does).
pub fn lc_state(id: &str) -> Option<String> {
    let o = spira_config::bounded::bounded("spira-lc").args(["state", id]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// lib.sh `spira_holder_witnesses <id>`: `Some(why)` when somebody may be home, `None` when
/// nobody is. The claim is the lifecycle row's: WORKING is somebody home, any other state is
/// the claim gone (sp-mve9i — bd's `in_progress` is not read), and no row is no claim at all
/// (a rowless bead can never be claimed). THE POSITIVE CONTROL: a machine that did not answer
/// cannot say the claim is gone, and that must never read as permission — so it counts as
/// "somebody may be home", not as absence.
pub fn holder_witnesses(run: &Path, id: &str, lc: &dyn ClaimProbe) -> Option<String> {
    if holder_alive(run, id) {
        return Some("a live process holds it".to_string());
    }
    match lc.probe(id) {
        Err(_) => Some("the lifecycle machine did not answer, so the claim witness proves nothing".to_string()),
        // No row: the machine answered that it has none, and a bead with no lifecycle row can
        // never be claimed — nobody is home (as for a branch whose bead never existed).
        Ok(None) => None,
        Ok(Some(row)) if row.working() => Some("WORKING — the lease has not been released".to_string()),
        Ok(Some(_)) => None,
    }
}

/// Salvage before destroying, and REFUSE TO DESTROY IF IT FAILS. `Ok(None)`: nothing to
/// save (a clean tree). `Ok(Some(path))`: the patch written. `Err(())`: salvage failed — the
/// removal must not proceed. Untracked files are carried by CONTENT, not by name, in a tar
/// beside the patch; the filename carries a timestamp so a second reap of the same bead does
/// not destroy the first salvage.
#[allow(clippy::result_unit_err)]
pub fn salvage(run: &Path, reaplog_path: &Path, id: &str, w: &Path) -> Result<Option<PathBuf>, ()> {
    let g = Git(w);
    // A worktree whose status cannot be read is not a clean one; it is a question. Fail
    // closed — the caller aborts its removal.
    let Some(dirty) = g.status_porcelain() else {
        reaplog(reaplog_path, "SALVAGE", id, &format!("cannot read the status of {} — refusing to call it clean", w.display()));
        return Err(());
    };
    if dirty.trim().is_empty() {
        return Ok(None);
    }
    let out_dir = run.join("reaped");
    if std::fs::create_dir_all(&out_dir).is_err() {
        reaplog(reaplog_path, "SALVAGE", id, &format!("cannot create {}", out_dir.display()));
        return Err(());
    }
    // %Y%m%dT%H%M%SZ — `date -u +%Y%m%dT%H%M%SZ`, built from the same clock read as
    // `utc_now` (YYYY-MM-DDTHH:MM:SSZ) with the punctuation stripped.
    let s = utc_now();
    let stamp = format!("{}{}{}T{}{}{}Z", &s[0..4], &s[5..7], &s[8..10], &s[11..13], &s[14..16], &s[17..19]);
    let base = out_dir.join(format!("{id}.{stamp}"));
    let patch_path = PathBuf::from(format!("{}.patch", base.display()));
    let untracked_name = format!("{id}.{stamp}.untracked.tar");

    let mut body = String::new();
    body.push_str(&format!("# {id} — uncommitted at reap time, {}\n", utc_now()));
    body.push_str(&format!("# tracked changes are below; untracked file CONTENT is in {untracked_name}\n"));
    body.push_str(&dirty);
    body.push_str("\n\n");
    body.push_str(&g.diff_head().unwrap_or_default());

    let mut rc_ok = !body.trim().is_empty();
    if rc_ok {
        rc_ok = std::fs::write(&patch_path, &body).is_ok();
    }

    let untracked = g.ls_files_others_nul();
    let mut wrote_tar = false;
    if !untracked.is_empty() {
        let tar_path = PathBuf::from(format!("{}.untracked.tar", base.display()));
        wrote_tar = tar_create(w, &untracked, &tar_path) && tar_path.metadata().map(|m| m.len() > 0).unwrap_or(false);
        if !wrote_tar {
            rc_ok = false;
        }
    }

    if !rc_ok {
        reaplog(reaplog_path, "SALVAGE", id, &format!("FAILED to write {} — the removal must not proceed", patch_path.display()));
        return Err(());
    }
    reaplog(
        reaplog_path,
        "SALVAGE",
        id,
        &format!("wrote {}{}", patch_path.display(), if wrote_tar { format!(" and {}.untracked.tar", base.display()) } else { String::new() }),
    );
    println!("  salvaged uncommitted changes to {}", patch_path.display());
    Ok(Some(patch_path))
}

fn tar_create(repo: &Path, nul_list: &[u8], out_tar: &Path) -> bool {
    // batch-job: tar runs for as long as its work does
    let child = Command::new("tar")
        .arg("-C")
        .arg(repo)
        .args(["--null", "-T", "-", "-cf"])
        .arg(out_tar)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return false };
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(nul_list);
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// lib.sh `spira_prune_worktrees <repo>`: `git worktree prune`, with the one case it gets
/// wrong — an entry is ALSO prunable when the worktree's `.git` file is missing while the
/// directory is entirely intact. Pruning that would free the branch for `git branch -D` and
/// leave a live tree registered nowhere. So: anything prune would drop whose directory still
/// exists is repaired instead, and if any live directory is STILL prunable after repair the
/// only safe move is not to prune at all.
pub fn prune_worktrees(reaplog_path: &Path, repo: &Path) -> bool {
    let g = Git(repo);
    let Some(common) = g.git_common_dir() else { return true };

    let prunable_path = |name: &str| -> Option<PathBuf> {
        let gd = std::fs::read_to_string(common.join("worktrees").join(name).join("gitdir")).ok()?;
        let p = gd.trim();
        let p = p.strip_suffix("/.git").unwrap_or(p);
        (!p.is_empty()).then(|| PathBuf::from(p))
    };
    let removing_names = |out: &str| -> Vec<String> {
        out.lines()
            .filter_map(|l| l.strip_prefix("Removing "))
            .map(|n| n.strip_prefix("worktrees/").unwrap_or(n))
            .map(|n| n.split(':').next().unwrap_or(n).to_string())
            .filter(|n| !n.is_empty())
            .collect()
    };

    for name in removing_names(&g.worktree_prune_dry()) {
        match prunable_path(&name) {
            Some(path) if path.is_dir() => {
                reaplog(reaplog_path, "REPAIRED", &name, &format!("prune would have dropped a worktree whose directory EXISTS at {} — repairing instead", path.display()));
                g.worktree_repair(&path);
            }
            _ => {
                reaplog(reaplog_path, "PRUNED", &name, "removed by git worktree prune");
            }
        }
    }

    let mut still = false;
    for name in removing_names(&g.worktree_prune_dry()) {
        if let Some(path) = prunable_path(&name) {
            if path.is_dir() {
                reaplog(reaplog_path, "REFUSED", &name, &format!("still prunable with its directory intact at {} — skipping the prune entirely", path.display()));
                still = true;
            }
        }
    }
    if still {
        return false;
    }
    g.worktree_prune();
    true
}

/// lib.sh `spira_destroy_worktree <id> <path> <repo> <why>`. `true`: removed, or nothing to
/// remove. `false`: refused or failed (a reap-log line names which).
pub fn destroy_worktree(run: &Path, reaplog_path: &Path, id: &str, w: &Path, repo: &Path, why: &str, bd: &dyn ClaimProbe) -> bool {
    if w.as_os_str().is_empty() {
        return true;
    }
    // ABSENT DIRECTORY FIRST — before the fence. Nothing on disk is left to remove; only a
    // dangling registration might remain, and `git worktree prune` touches nothing on disk,
    // so it is safe regardless of where the path points.
    if !w.exists() {
        prune_worktrees(reaplog_path, repo);
        return true;
    }
    // THE FENCE. Guards only paths whose directories exist; the absent case is handled above.
    let wt_root = run.join("worktree");
    if !(w.starts_with(&wt_root) && w != wt_root) {
        reaplog(reaplog_path, "REFUSED", id, &format!("{} is not under {} — refusing to remove it", w.display(), wt_root.display()));
        return false;
    }
    if let Some(held) = holder_witnesses(run, id, bd) {
        reaplog(reaplog_path, "REFUSED", id, &format!("worktree {} — {held}", w.display()));
        return false;
    }
    // A WORKTREE PATH IS KEYED ON THE BEAD (its directory name), independent of whichever
    // branch happens to be checked out inside it (sp-87csm): check the path's own id too
    // whenever it differs from the caller's.
    let path_id = w.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if path_id != id {
        if let Some(held) = holder_witnesses(run, &path_id, bd) {
            reaplog(reaplog_path, "REFUSED", id, &format!("worktree {} — {held} (as {path_id})", w.display()));
            return false;
        }
    }
    if salvage(run, reaplog_path, id, w).is_err() {
        reaplog(reaplog_path, "REFUSED", id, &format!("worktree {} — salvage failed, so the removal is abandoned", w.display()));
        return false;
    }
    // Logged BEFORE the act as well as after: a process killed between the two leaves a
    // record that it was about to delete this tree.
    reaplog(reaplog_path, "REMOVING", id, &format!("worktree {} ({why})", w.display()));
    let g = Git(repo);
    if !g.worktree_remove_force(w) {
        let _ = std::fs::remove_dir_all(w);
        prune_worktrees(reaplog_path, repo);
    }
    if w.exists() {
        reaplog(reaplog_path, "FAILED", id, &format!("worktree {} survived removal", w.display()));
        return false;
    }
    reaplog(reaplog_path, "REMOVED", id, &format!("worktree {}", w.display()));
    true
}

/// Why `destroy_branch` refused or failed; `CertifiedQueued` is the one callers branch on
/// (the batch/certify path, never bypassed).
pub enum DestroyBranchErr {
    Refused(String),
    CertifiedQueued,
    Failed(String),
}

/// lib.sh `spira_destroy_branch <id> <branch> <repo> <why> [caller]`. `caller` non-empty
/// (`"sending"`, `"slain"`, `"archived"`, or any other value a future caller uses) means the
/// caller has already verified safety by its own means and the content fence is skipped;
/// empty applies it. CONTENT, NOT ANCESTRY: the fence asks the same question the Sending's
/// own selector asks (`content_on_base`), never `merge-base --is-ancestor`, or the two sides
/// contradict on an empty-commit branch.
#[allow(clippy::too_many_arguments)]
pub fn destroy_branch(run: &Path, reaplog_path: &Path, id: &str, br: &str, repo: &Path, why: &str, caller: &str, base: Option<&str>, bd: &dyn ClaimProbe) -> Result<(), DestroyBranchErr> {
    let g = Git(repo);
    if !g.branch_exists(br) {
        return Ok(());
    }
    // CERTIFIED/BATCHED GUARD. No caller bypass: even the Sending must not race verdict.sh's
    // LANDED write.
    if let Some(state) = lc_state(id).filter(|s| is_queued_state(s)) {
        reaplog(reaplog_path, "REFUSED", id, &format!("branch {br} — lifecycle state is {state}; not deleting a queued branch"));
        return Err(DestroyBranchErr::CertifiedQueued);
    }
    if let Some(held) = holder_witnesses(run, id, bd) {
        reaplog(reaplog_path, "REFUSED", id, &format!("branch {br} — {held}"));
        return Err(DestroyBranchErr::Refused(held));
    }
    // A branch a worktree still holds is not deletable, and forcing the issue by pruning the
    // registration out from under it is how a live tree becomes an orphan.
    if let Some(wt) = g.worktree_of(br) {
        if wt.exists() {
            let msg = format!("branch {br} is checked out at {}", wt.display());
            reaplog(reaplog_path, "REFUSED", id, &msg);
            return Err(DestroyBranchErr::Refused(msg));
        }
    }
    // CONTENT FENCE. Fires only when the caller has not already verified safety, and only
    // when the base resolves — exactly as lib.sh's `&&` chain: an unresolvable base skips
    // the fence rather than refusing (a caller that could not even ask the question, not an
    // answer to it).
    if caller.is_empty() {
        if let Some(base) = base {
            if g.verify(base).is_some() && !g.content_on_base(br, base) {
                let msg = format!("branch {br} — content not on {base}, refusing to destroy unlanded work ({why})");
                reaplog(reaplog_path, "REFUSED", id, &msg);
                return Err(DestroyBranchErr::Refused(msg));
            }
        }
    }
    reaplog(reaplog_path, "REMOVING", id, &format!("branch {br} ({why})"));
    // SPIRA_REF_SANCTIONED is what the reference-transaction hook reads, set ONLY on this
    // one command: every guard that makes this deletion safe has already run by this line.
    let err = g.branch_delete_sanctioned(br);
    if g.branch_exists(br) {
        reaplog(reaplog_path, "FAILED", id, &format!("branch {br} survived deletion: {err}"));
        return Err(DestroyBranchErr::Failed(err));
    }
    reaplog(reaplog_path, "REMOVED", id, &format!("branch {br}"));
    Ok(())
}

/// The outcome of `reap_landed_branch`. Unlike `sending`'s own `World::send` (which rechecks
/// the witness in the same breath so nothing can claim the bead between the two, and so can
/// be `Held`), this function itself never reports held — a caller who wants that recheck
/// does it with `holder_witnesses` immediately before calling this.
pub enum ReapOutcome {
    Done,
    Queued,
    Failed(String),
}

/// lib.sh `spira_reap_landed_branch <id> <branch> <repo> <why> [caller]`: the one sequence
/// every deleter of a landed branch shares — worktree first (git refuses to delete a branch
/// a worktree still holds), then the branch, then its remote counterpart, then the
/// `branch:<br>` label (law-branch-affinity-is-recorded: the label goes with the ref, or a
/// bead reopened after this is handed the name of a ref that no longer exists).
#[allow(clippy::too_many_arguments)]
pub fn reap_landed_branch(
    run: &Path,
    reaplog_path: &Path,
    id: &str,
    br: &str,
    repo: &Path,
    why: &str,
    caller: &str,
    base: Option<&str>,
    remote: Option<&str>,
    bd: &dyn ClaimProbe,
    log: &dyn Fn(&str),
    label_remove: &dyn Fn(&str, &str),
) -> ReapOutcome {
    let g = Git(repo);
    if let Some(w) = g.worktree_of(br) {
        if !destroy_worktree(run, reaplog_path, id, &w, repo, why, bd) {
            return ReapOutcome::Failed(format!("worktree {} was not removed — see {}", w.display(), reaplog_path.display()));
        }
    }
    match destroy_branch(run, reaplog_path, id, br, repo, why, caller, base, bd) {
        Err(DestroyBranchErr::CertifiedQueued) => return ReapOutcome::Queued,
        Err(DestroyBranchErr::Refused(m)) | Err(DestroyBranchErr::Failed(m)) => {
            let detail = if m.is_empty() {
                format!("refused — content not landed, held, or checked out; see {}", reaplog_path.display())
            } else {
                m
            };
            return ReapOutcome::Failed(format!("branch {br} not deleted: {detail}"));
        }
        Ok(()) => {}
    }
    if let Some(rem) = remote {
        if g.verify(&format!("{rem}/{br}")).is_some() && g.push_delete(rem, br) {
            log(&format!("reap {id}: deleted {rem}/{br}"));
        }
    }
    label_remove(id, &format!("branch:{br}"));
    ReapOutcome::Done
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as Cmd;
    use testkit::TempDir;

    fn tempdir() -> TempDir {
        TempDir::new("sending-reap-test")
    }

    /// The lifecycle row's state for every bead (`None`: the machine did not answer; an
    /// empty state: no row).
    struct FakeBd {
        state: Option<String>,
    }
    impl ClaimProbe for FakeBd {
        fn probe(&self, id: &str) -> Result<Option<spira_config::lc_state::Row>, String> {
            match &self.state {
                None => Err("cannot tell".into()),
                Some(s) if s.is_empty() => Ok(None),
                Some(s) => Ok(Some(spira_config::lc_state::Row { bead_id: id.into(), state: s.clone(), ..Default::default() })),
            }
        }
    }
    fn open() -> FakeBd {
        FakeBd { state: Some("READY".into()) }
    }

    fn git(dir: &Path, args: &[&str]) {
        let o = Cmd::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("f"), "base\n").unwrap();
        git(dir, &["add", "f"]);
        git(dir, &["commit", "-q", "-m", "base"]);
    }

    // ---- hold_alive / holder_alive -----------------------------------------------------

    #[test]
    fn hold_alive_sees_a_live_pid_and_rejects_dead_or_missing() {
        let t = tempdir();
        let live = t.join("live.pid");
        std::fs::write(&live, std::process::id().to_string()).unwrap();
        assert!(hold_alive(&live));

        let dead = t.join("dead.pid");
        std::fs::write(&dead, "999999999").unwrap();
        assert!(!hold_alive(&dead));

        assert!(!hold_alive(&t.join("missing.pid")));
    }

    #[test]
    fn holder_alive_checks_the_hold_pidfile_by_pid_only() {
        let run = tempdir();
        std::fs::write(run.join("hold-sp-h1.pid"), std::process::id().to_string()).unwrap();
        assert!(holder_alive(&run, "sp-h1"));
        assert!(!holder_alive(&run, "sp-h2"));
    }

    #[test]
    fn holder_alive_requires_aeon_argv_for_an_aeon_pidfile() {
        let run = tempdir();
        // A live pid whose argv is NOT an aeon must not count — a recycled pid must not
        // resurrect a dead aeon's claim.
        std::fs::write(run.join("aeon-builder-sp-a1.pid"), std::process::id().to_string()).unwrap();
        assert!(!holder_alive(&run, "sp-a1"));
    }

    // ---- reaplog -------------------------------------------------------------------------

    #[test]
    fn reaplog_appends_one_line_naming_verb_id_and_detail() {
        let run = tempdir();
        let path = run.join("reap.log");
        reaplog(&path, "REFUSED", "sp-x", "some detail");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("REFUSED"), "{content}");
        assert!(content.contains("sp-x"), "{content}");
        assert!(content.contains("some detail"), "{content}");
        assert!(content.contains("[by "), "{content}");
    }

    // ---- destroy_worktree ----------------------------------------------------------------

    #[test]
    fn destroy_worktree_prunes_and_succeeds_on_an_absent_path_outside_root() {
        let run = tempdir();
        let repo = tempdir();
        init_repo(&repo);
        let outside = run.join("not-the-worktree-root/sp-test");
        assert!(destroy_worktree(&run, &run.join("reap.log"), "sp-test", &outside, &repo, "why", &open()));
    }

    #[test]
    fn destroy_worktree_refuses_a_live_out_of_root_path() {
        let run = tempdir();
        let outside = tempdir(); // exists, and is NOT under run/worktree
        let repo = tempdir();
        init_repo(&repo);
        let ok = destroy_worktree(&run, &run.join("reap.log"), "sp-test2", &outside, &repo, "why", &open());
        assert!(!ok);
        let log = std::fs::read_to_string(run.join("reap.log")).unwrap_or_default();
        assert!(log.contains("REFUSED"), "{log}");
        assert!(log.contains("not under"), "{log}");
    }

    #[test]
    fn destroy_worktree_refuses_while_held() {
        let run = tempdir();
        let repo = tempdir();
        init_repo(&repo);
        let wt = run.join("worktree").join("sp-held");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(run.join("hold-sp-held.pid"), std::process::id().to_string()).unwrap();
        let ok = destroy_worktree(&run, &run.join("reap.log"), "sp-held", &wt, &repo, "why", &open());
        assert!(!ok);
        assert!(wt.exists());
    }

    #[test]
    fn destroy_worktree_removes_a_real_worktree_under_root() {
        let run = tempdir();
        let repo = tempdir();
        init_repo(&repo);
        git(&repo, &["branch", "spira/sp-wt1"]);
        let wt = run.join("worktree").join("sp-wt1");
        std::fs::create_dir_all(wt.parent().unwrap()).unwrap();
        git(&repo, &["worktree", "add", "-q", wt.to_str().unwrap(), "spira/sp-wt1"]);
        assert!(wt.exists());
        let ok = destroy_worktree(&run, &run.join("reap.log"), "sp-wt1", &wt, &repo, "test", &open());
        assert!(ok);
        assert!(!wt.exists());
    }

    // ---- destroy_branch --------------------------------------------------------------------

    #[test]
    fn only_certified_and_in_delivery_are_queued_states() {
        assert!(is_queued_state("CERTIFIED") && is_queued_state("IN_DELIVERY"));
        assert!(!is_queued_state("WORKING") && !is_queued_state("LANDED") && !is_queued_state(""));
    }

    #[test]
    fn destroy_branch_content_fence_refuses_unlanded_work_and_allows_landed() {
        let run = tempdir();
        let repo = tempdir();
        init_repo(&repo);
        let base = Git(&repo).rev_parse("HEAD").unwrap();

        // Unlanded: a real new commit the base does not have.
        git(&repo, &["checkout", "-q", "-b", "spira/sp-un"]);
        std::fs::write(repo.join("new.txt"), "x\n").unwrap();
        git(&repo, &["add", "new.txt"]);
        git(&repo, &["commit", "-q", "-m", "unlanded"]);
        git(&repo, &["checkout", "-q", "main"]);
        let res = destroy_branch(&run, &run.join("reap.log"), "sp-un", "spira/sp-un", &repo, "why", "", Some(&base), &open());
        assert!(res.is_err());
        assert!(Git(&repo).branch_exists("spira/sp-un"));

        // Landed: an empty branch off the same base (trivially an ancestor).
        git(&repo, &["branch", "spira/sp-empty", &base]);
        let res = destroy_branch(&run, &run.join("reap.log"), "sp-empty", "spira/sp-empty", &repo, "why", "", Some(&base), &open());
        assert!(res.is_ok(), "{res:?}");
        assert!(!Git(&repo).branch_exists("spira/sp-empty"));
    }

    #[test]
    fn destroy_branch_caller_bypass_skips_the_content_fence() {
        let run = tempdir();
        let repo = tempdir();
        init_repo(&repo);
        git(&repo, &["checkout", "-q", "-b", "spira/sp-bypass"]);
        std::fs::write(repo.join("new.txt"), "x\n").unwrap();
        git(&repo, &["add", "new.txt"]);
        git(&repo, &["commit", "-q", "-m", "unlanded but caller already verified"]);
        git(&repo, &["checkout", "-q", "main"]);
        let res = destroy_branch(&run, &run.join("reap.log"), "sp-bypass", "spira/sp-bypass", &repo, "why", "sending", None, &open());
        assert!(res.is_ok());
        assert!(!Git(&repo).branch_exists("spira/sp-bypass"));
    }

    impl std::fmt::Debug for DestroyBranchErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                DestroyBranchErr::Refused(m) => write!(f, "Refused({m})"),
                DestroyBranchErr::CertifiedQueued => write!(f, "CertifiedQueued"),
                DestroyBranchErr::Failed(m) => write!(f, "Failed({m})"),
            }
        }
    }

    // ---- holder_witnesses ------------------------------------------------------------------

    #[test]
    fn holder_witnesses_reads_the_lifecycle_claim_and_reflects_the_outage() {
        let run = tempdir();
        let working = FakeBd { state: Some("WORKING".into()) };
        assert_eq!(holder_witnesses(&run, "sp-w1", &working).as_deref(), Some("WORKING — the lease has not been released"));

        let unreachable = FakeBd { state: None };
        assert_eq!(holder_witnesses(&run, "sp-w2", &unreachable).as_deref(), Some("the lifecycle machine did not answer, so the claim witness proves nothing"));

        assert_eq!(holder_witnesses(&run, "sp-w3", &open()), None);
        // Past the builder, the claim is gone whatever bd's status still says.
        assert_eq!(holder_witnesses(&run, "sp-w4", &FakeBd { state: Some("SUBMITTED".into()) }), None);
        // No row: a bead the machine has no row for can never be claimed (lifecycle_row's
        // doc), so nobody can be home — as a branch with no bead at all never had a claim.
        // The machine answered; only an unanswered probe is the positive control.
        assert_eq!(holder_witnesses(&run, "sp-w5", &FakeBd { state: Some(String::new()) }), None);
    }

}
