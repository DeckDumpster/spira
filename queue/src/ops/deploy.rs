//! land-local's checkout deploy (DESIGN.md §8 D11): when production runs a checkout (no
//! release in force) and the landing repository IS that checkout, move it to the landed
//! head — stage-and-swap, `reset --mixed`, a HEAD re-read — install the round's own
//! binaries, and smoke the checkout's conf.sh.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::World;
use crate::ports::{ref_branch, TreeEntry};

/// The allow-list of checkout-relative paths whose tracked edits a deploy may overwrite
/// (the files active overrides re-apply). Colon-separated, exact paths.
pub const ALLOW_VAR: &str = "SPIRA_LAND_DEPLOY_ALLOW";

const GITLINK: &str = "160000";

/// Everything the deploy will do, fixed before the CAS.
#[derive(Debug)]
pub struct Plan {
    pub checkout: PathBuf,
    /// The checkout's HEAD before the land.
    pub old: String,
    /// Paths to write, with their entry at the head.
    pub writes: Vec<(String, TreeEntry)>,
    /// Paths the head no longer has.
    pub deletes: Vec<String>,
    /// The round's own `target/release`.
    pub bins: PathBuf,
}

fn canon(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The running harness checkout, when it is `repo`: `$SPIRA_HOME`'s work-tree top level
/// equals the landing repository's path. None for any other repository.
pub fn running_checkout(w: &World, home: &Path, repo: &Path) -> Option<PathBuf> {
    let top = w.git.toplevel(home)?;
    (canon(&top) == canon(repo)).then(|| repo.to_path_buf())
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(9)]
}

/// D11 step 1: every precondition, before the CAS. Err is the refusal reason.
pub fn prepare(w: &World, checkout: &Path, landref: &str, head: &str, bins: Option<&Path>) -> Result<Plan, String> {
    let Some(bins) = bins else {
        return Err(format!(
            "--worktree <round worktree> is required to deploy the production checkout {} — its binaries must be the round's own build",
            checkout.display()
        ));
    };
    let want = ref_branch(landref);
    let on = w.git.current_branch(checkout);
    if on.as_deref() != Some(want) {
        return Err(format!(
            "the production checkout {} is not on {want} (it is on {}) — cannot deploy",
            checkout.display(),
            on.unwrap_or_else(|| "a detached HEAD".into())
        ));
    }
    let Some(old) = w.git.rev_parse(checkout, "HEAD") else {
        return Err(format!("cannot read the production checkout's HEAD ({})", checkout.display()));
    };
    if !w.git.is_ancestor(checkout, &old, head) {
        return Err(format!("the production checkout's HEAD {} is not an ancestor of {head} — cannot deploy by fast-forward", short(&old)));
    }
    let dirty = w.git.tracked_changes(checkout).map_err(|e| format!("cannot read the production checkout's status: {e}"))?;
    let allow: Vec<String> = w.var(ALLOW_VAR).map(|v| v.split(':').filter(|p| !p.is_empty()).map(str::to_string).collect()).unwrap_or_default();
    let unexpected: Vec<&String> = dirty.iter().filter(|p| !allow.contains(p)).collect();
    if !unexpected.is_empty() {
        let shown: Vec<&str> = unexpected.iter().take(5).map(|s| s.as_str()).collect();
        return Err(format!(
            "the production checkout has {} tracked edit(s) outside {ALLOW_VAR}: {}{}",
            unexpected.len(),
            shown.join(", "),
            if unexpected.len() > 5 { ", …" } else { "" }
        ));
    }
    let a = w.git.tree(checkout, &old).map_err(|e| format!("cannot list the tree at {}: {e}", short(&old)))?;
    let b = w.git.tree(checkout, head).map_err(|e| format!("cannot list the tree at {}: {e}", short(head)))?;
    let (writes, deletes) = diff(&a, &b);
    if let Some((p, _)) = writes.iter().find(|(p, e)| e.mode == GITLINK || a.get(p).is_some_and(|o| o.mode == GITLINK)) {
        return Err(format!("{p} is a submodule change — a gitlink cannot be deployed by swap"));
    }
    if let Some(p) = deletes.iter().find(|p| a[*p].mode == GITLINK) {
        return Err(format!("{p} is a submodule removal — a gitlink cannot be deployed by swap"));
    }
    Ok(Plan { checkout: checkout.to_path_buf(), old, writes, deletes, bins: bins.to_path_buf() })
}

/// Paths whose entry differs (or is new) at `b`, and paths `b` no longer has.
fn diff(a: &BTreeMap<String, TreeEntry>, b: &BTreeMap<String, TreeEntry>) -> (Vec<(String, TreeEntry)>, Vec<String>) {
    let writes = b.iter().filter(|(p, e)| a.get(*p) != Some(e)).map(|(p, e)| (p.clone(), e.clone())).collect();
    let deletes = a.keys().filter(|p| !b.contains_key(*p)).cloned().collect();
    (writes, deletes)
}

fn beside(target: &Path, tag: &str) -> PathBuf {
    let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    target.with_file_name(format!(".{name}.{tag}.{}", std::process::id()))
}

/// Write one entry beside its target with the tree's exact mode, then rename over it.
fn swap_one(w: &World, checkout: &Path, path: &str, e: &TreeEntry) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let target = checkout.join(path);
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    }
    let data = w.git.blob(checkout, &e.sha).map_err(|err| format!("cannot read {path} ({}) from git: {err}", e.sha))?;
    let tmp = beside(&target, "land-new");
    let _ = fs::remove_file(&tmp);
    let written = match e.mode.as_str() {
        "120000" => {
            let to = String::from_utf8_lossy(&data).to_string();
            std::os::unix::fs::symlink(&to, &tmp).map_err(|err| err.to_string())
        }
        m => {
            let mode = if m == "100755" { 0o755 } else { 0o644 };
            fs::write(&tmp, &data)
                .and_then(|()| fs::set_permissions(&tmp, fs::Permissions::from_mode(mode)))
                .map_err(|err| err.to_string())
        }
    };
    let r = written.and_then(|()| fs::rename(&tmp, &target).map_err(|err| err.to_string()));
    if let Err(err) = r {
        let _ = fs::remove_file(&tmp);
        return Err(format!("cannot write {path}: {err}"));
    }
    Ok(())
}

fn delete_one(checkout: &Path, path: &str) -> Result<(), String> {
    let target = checkout.join(path);
    match fs::remove_file(&target) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot delete {path}: {e}")),
    }
    // Emptied parent directories go too (as a checkout would leave them), never the root.
    let mut dir = target.parent();
    while let Some(d) = dir {
        if d == checkout || !d.starts_with(checkout) || fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
    Ok(())
}

/// D11 steps 2-5, after the CAS. Returns false when anything failed (already reported
/// loudly); nothing here reverts the ref.
pub fn run(w: &World, plan: &Plan, head: &str, home: &Path, lifecycle_on: bool) -> bool {
    let checkout = &plan.checkout;
    let total = plan.writes.len() + plan.deletes.len();
    let mut done = 0usize;
    let mut failure = None;
    for (p, e) in &plan.writes {
        if let Err(why) = swap_one(w, checkout, p, e) {
            failure = Some(why);
            break;
        }
        done += 1;
    }
    if failure.is_none() {
        for p in &plan.deletes {
            if let Err(why) = delete_one(checkout, p) {
                failure = Some(why);
                break;
            }
            done += 1;
        }
    }
    if let Some(why) = failure {
        w.err(format!(
            "LAND DEPLOY FAILED: {why} — {done} of {total} file(s) swapped; the production checkout {} is NOT reset (HEAD still {}), binaries and smoke skipped; the landing ref is already at {head} and is not reverted",
            checkout.display(),
            short(&plan.old)
        ));
        return false;
    }
    if !w.git.reset_mixed(checkout, head) {
        w.err(format!("LAND DEPLOY FAILED: git reset --mixed {head} failed in {} after all {total} file(s) were swapped", checkout.display()));
        return false;
    }
    let now = w.git.rev_parse(checkout, "HEAD");
    if now.as_deref() != Some(head) {
        w.err(format!(
            "LAND DEPLOY FAILED: the production checkout's HEAD reads {} after the reset, not {head} — binaries and smoke skipped",
            now.as_deref().unwrap_or("<unreadable>")
        ));
        return false;
    }

    let (installed, bins_ok) = install_bins(w, &plan.bins, checkout, lifecycle_on);
    let smoke_ok = smoke(w, home, head);
    w.out(format!("queue.sh land-local: production checkout {} -> {}, {total} files, {installed} binaries", short(&plan.old), short(head)));
    bins_ok && smoke_ok
}

/// D11 step 3: every regular executable directly in the round's `target/release`, copied
/// beside its target in `<checkout>/target/release` and renamed over it, and linked (or
/// re-linked) at `<checkout>/bin/<name>` — the path `spira_bin` actually resolves
/// (conf.sh: `$SPIRA_REPO/bin/$name`). Before this, only the copy happened: an existing
/// `bin/<name>` symlink kept working because the file its target already named was
/// overwritten in place, but a binary landing for the first time had no symlink to update
/// and `spira_bin` could never find it (sp-cln99, sp-sghmt). spira-lc only with
/// lifecycle_enforce on.
///
/// Fail closed: every binary the workspace's own `[[bin]]` targets declare
/// ([`expected_bins`]) must be among what the round actually built, or the deploy refuses
/// rather than link whatever happened to be there.
fn install_bins(w: &World, from: &Path, checkout: &Path, lifecycle_on: bool) -> (usize, bool) {
    let to = checkout.join("target").join("release");
    let bin_dir = checkout.join("bin");
    let mut entries: Vec<PathBuf> = match fs::read_dir(from) {
        Ok(rd) => rd.flatten().map(|e| e.path()).filter(|p| super::simple::is_executable(p)).collect(),
        Err(e) => {
            w.err(format!("LAND DEPLOY FAILED: cannot read the round's binaries at {}: {e}", from.display()));
            return (0, false);
        }
    };
    entries.sort();
    if let Err(e) = fs::create_dir_all(&to) {
        w.err(format!("LAND DEPLOY FAILED: cannot create {}: {e}", to.display()));
        return (0, false);
    }
    let mut got: BTreeSet<String> = BTreeSet::new();
    let (mut n, mut ok) = (0, true);
    for src in entries {
        let Some(name) = src.file_name().map(|n| n.to_string_lossy().to_string()) else { continue };
        if name == "spira-lc" && !lifecycle_on {
            continue;
        }
        let dst = to.join(&name);
        let tmp = beside(&dst, "land-new");
        let r = fs::copy(&src, &tmp).and_then(|_| fs::rename(&tmp, &dst));
        match r {
            Ok(()) => {
                n += 1;
                got.insert(name.clone());
                if let Err(e) = link_bin(&bin_dir, &name, &dst) {
                    w.err(format!("LAND DEPLOY FAILED: cannot link bin/{name} -> {}: {e}", dst.display()));
                    ok = false;
                }
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                w.err(format!("LAND DEPLOY FAILED: cannot install {name} into {}: {e}", to.display()));
                ok = false;
            }
        }
    }

    if let Some(worktree) = from.parent().and_then(Path::parent) {
        match expected_bins(worktree) {
            Ok(expected) => {
                let missing: Vec<&String> = expected
                    .iter()
                    .filter(|name| name.as_str() != "spira-lc" || lifecycle_on)
                    .filter(|name| !got.contains(name.as_str()))
                    .collect();
                if !missing.is_empty() {
                    let names: Vec<&str> = missing.iter().map(|s| s.as_str()).collect();
                    w.err(format!(
                        "LAND DEPLOY FAILED: the workspace's [[bin]] targets declare {} but the round's build at {} does not have {}: refusing a partial binary set",
                        names.join(", "),
                        from.display(),
                        if missing.len() == 1 { "it" } else { "them" }
                    ));
                    ok = false;
                }
            }
            Err(e) => {
                w.err(format!("LAND DEPLOY FAILED: cannot determine the workspace's [[bin]] targets from {}: {e}", worktree.display()));
                ok = false;
            }
        }
    }

    (n, ok)
}

/// Every binary name the workspace's own `[[bin]]` targets declare, read from
/// `<worktree>/Cargo.toml`'s `[workspace] members` and each member's own `Cargo.toml`:
/// its explicit `[[bin]]` tables, or (when it has none) the package name when
/// `src/main.rs` exists (cargo's own default). THE SOURCE OF TRUTH, not the Makefile's
/// `install` list — that list is hand-maintained prose and had already drifted as of
/// 2026-09-29 (missing reconciler-alert, lifecycle-guard, spira-lc and test-plan, all
/// already built and already running). A list that must be remembered and edited by hand
/// for every new binary crate is exactly the failure mode this bead exists to close: the
/// workspace's own manifests cannot go stale relative to what cargo actually builds.
fn expected_bins(worktree: &Path) -> Result<Vec<String>, String> {
    let root_toml = worktree.join("Cargo.toml");
    let root = read_toml(&root_toml)?;
    let members = root
        .get("workspace")
        .and_then(|v| v.get("members"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("{} has no [workspace].members array", root_toml.display()))?;
    let mut names = Vec::new();
    for m in members {
        let rel = m.as_str().ok_or_else(|| format!("{} has a non-string workspace member", root_toml.display()))?;
        let dir = worktree.join(rel);
        let manifest = dir.join("Cargo.toml");
        let doc = read_toml(&manifest)?;
        let bins = doc.get("bin").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        if bins.is_empty() {
            if dir.join("src").join("main.rs").is_file() {
                let name = doc
                    .get("package")
                    .and_then(|p| p.get("name"))
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| format!("{} has src/main.rs but no [package].name", manifest.display()))?;
                names.push(name.to_string());
            }
        } else {
            for b in &bins {
                let name = b.get("name").and_then(|n| n.as_str()).ok_or_else(|| format!("{} has a [[bin]] with no name", manifest.display()))?;
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn read_toml(path: &Path) -> Result<toml::Value, String> {
    let src = fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    src.parse::<toml::Value>().map_err(|e| format!("cannot parse {}: {e}", path.display()))
}

/// Atomically make `<bin_dir>/<name>` a symlink to `target` (`<checkout>/target/release/<name>`,
/// the path `spira_bin` resolves in production). Idempotent: a link already pointing there
/// is left alone.
fn link_bin(bin_dir: &Path, name: &str, target: &Path) -> Result<(), String> {
    fs::create_dir_all(bin_dir).map_err(|e| format!("cannot create {}: {e}", bin_dir.display()))?;
    let link = bin_dir.join(name);
    if fs::read_link(&link).ok().as_deref() == Some(target) {
        return Ok(());
    }
    let tmp = beside(&link, "land-bin");
    let _ = fs::remove_file(&tmp);
    if let Err(e) = std::os::unix::fs::symlink(target, &tmp) {
        return Err(e.to_string());
    }
    if let Err(e) = fs::rename(&tmp, &link) {
        let _ = fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(())
}

/// D11 step 4: the checkout's own conf.sh must still resolve SPIRA_DB to a directory.
fn smoke(w: &World, home: &Path, head: &str) -> bool {
    let got = match w.lib.conf_smoke(home) {
        Ok(db) => db,
        Err(e) => format!("<smoke did not run: {e}>"),
    };
    if !got.is_empty() && Path::new(&got).is_dir() {
        return true;
    }
    w.err(format!(
        "LAND SMOKE FAILED: production conf.sh no longer resolves SPIRA_DB (got '{got}') — the landing ref and the checkout are already at {head} and are NOT reverted; fix before anything else"
    ));
    false
}
