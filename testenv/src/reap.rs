//! `target-reap` (sp-z61hj, then sp-x9kbg; spira-config/DESIGN-build-cache.md §2.4): remove
//! the `target/` of a worktree whose branch has landed. Build output of finished work is the
//! largest thing on the box's disk (236 GiB measured 2026-09-30, then 87 GB and 80 GB more
//! removed by hand the day sp-x9kbg was filed) and nothing reads it again.
//!
//! Candidates are `<worktrees>/<bead id>/target` and `<worktrees>/concierge-<bead id>/target`
//! — the aeon and the Concierge's own worktree conventions. Gate trees and testenv slots are
//! owned by the gate and testenv. The gate is per worktree, decided by [`crate::landed`]: a
//! bead the lifecycle record has LANDED (`spira-lc state`) is reaped — whatever bd's status
//! says, so a bead that stays open past landing no longer holds the target/ hostage
//! (sp-x9kbg). A fresh worktree is WORKING on the record, never LANDED, so its tip trivially
//! equalling the base it was cut from no longer matters (the same bead's second defect —
//! round 198 reaped two in-progress aeons). Anything [`crate::landed`] cannot tell — the
//! record unreachable — is kept (fail closed — never guess that
//! work is finished), and so is anything a live process still has open ([`crate::busy`]), a
//! second, structural guard independent of what landed-ness concluded.

use std::fs;
use std::path::{Path, PathBuf};

/// A bead id as a worktree is named after it: `sp-<base36>` with optional `.<n>` children.
pub fn is_bead_id(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("sp-") else { return false };
    let mut parts = rest.split('.');
    let head = parts.next().unwrap_or("");
    !head.is_empty()
        && head.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// The bead id a worktree directory is named after — bare (`sp-x9kbg`, the aeon convention)
/// or Concierge-prefixed (`concierge-sp-x9kbg`) — or `None` when the name is neither (a gate
/// tree, a testenv slot, a round's own scratch dir: none of this crate's business).
pub fn worktree_id(dirname: &str) -> Option<&str> {
    let core = dirname.strip_prefix("concierge-").unwrap_or(dirname);
    is_bead_id(core).then_some(core)
}

/// `(bead id, worktree dir, <dir>/target)` for every bead-named worktree under `worktrees`
/// that holds a `target` (a directory or a link), sorted by id (then by dir, for the rare
/// case the same bead has both an aeon and a Concierge worktree at once).
pub fn candidates(worktrees: &Path) -> Vec<(String, PathBuf, PathBuf)> {
    let mut out: Vec<(String, PathBuf, PathBuf)> = fs::read_dir(worktrees)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let dirname = e.file_name().to_string_lossy().into_owned();
                    let id = worktree_id(&dirname)?.to_string();
                    let dir = e.path();
                    let t = dir.join("target");
                    (dir.is_dir() && fs::symlink_metadata(&t).is_ok()).then_some((id, dir, t))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Bytes under `p`, not following links.
pub fn size_of(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(md) = fs::symlink_metadata(p) else { return 0 };
    if !md.is_dir() {
        return md.blocks() * 512;
    }
    fs::read_dir(p).map(|rd| rd.flatten().map(|e| size_of(&e.path())).sum()).unwrap_or(0)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reaped {
    pub removed: Vec<String>,
    pub bytes: u64,
    /// Proven NOT landed — commits outstanding, or a fresh worktree the landing ref never
    /// named. NEVER reaped, whatever the bead says.
    pub kept_unlanded: usize,
    /// Landed-ness could not be told at all (no git answer, no resolvable landing ref).
    pub kept_unknown: Vec<String>,
    /// Landed, but a live process still has it open ([`crate::busy`]) — the second guard.
    pub kept_busy: usize,
    /// Idle pass: something outside `target/` changed inside the idle window.
    pub kept_active: usize,
}

/// Reap: `lc_landed(id, worktree_dir)` answers per worktree — `Some(true)` the lifecycle
/// record has the bead LANDED, `Some(false)` it has it in any other state (or no row at
/// all), `None` it cannot be told. Only `Some(true)` is a candidate for removal, and even
/// then only when `busy(worktree_dir)` says no live process still has it open.
pub fn reap(
    worktrees: &Path,
    dry_run: bool,
    lc_landed: &dyn Fn(&str, &Path) -> Option<bool>,
    busy: &dyn Fn(&Path) -> bool,
) -> Result<Reaped, String> {
    let mut out = Reaped::default();
    for (id, dir, t) in candidates(worktrees) {
        match lc_landed(&id, &dir) {
            Some(true) => {
                if busy(&dir) {
                    out.kept_busy += 1;
                    continue;
                }
                let n = size_of(&t);
                if !dry_run {
                    let r = if fs::symlink_metadata(&t).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                        fs::remove_file(&t)
                    } else {
                        fs::remove_dir_all(&t)
                    };
                    if let Err(e) = r {
                        return Err(format!("cannot remove {}: {e}", t.display()));
                    }
                }
                out.bytes += n;
                out.removed.push(id);
            }
            Some(false) => out.kept_unlanded += 1,
            None => out.kept_unknown.push(id),
        }
    }
    Ok(out)
}

/// Seconds since the newest modification of anything in `dir` outside its top-level
/// `target/`, not following links. `None` when nothing could be read — the caller keeps it.
pub fn idle_age_secs(dir: &Path) -> Option<u64> {
    use std::time::SystemTime;
    fn newest(p: &Path, top: bool, acc: &mut Option<SystemTime>) {
        let Ok(rd) = fs::read_dir(p) else { return };
        for e in rd.flatten() {
            if top && e.file_name() == "target" {
                continue;
            }
            let Ok(md) = fs::symlink_metadata(e.path()) else { continue };
            if let Ok(m) = md.modified() {
                if acc.map_or(true, |a| m > a) {
                    *acc = Some(m);
                }
            }
            if md.is_dir() {
                newest(&e.path(), false, acc);
            }
        }
    }
    let mut acc = fs::symlink_metadata(dir).ok()?.modified().ok();
    newest(dir, true, &mut acc);
    Some(SystemTime::now().duration_since(acc?).map(|d| d.as_secs()).unwrap_or(0))
}

/// Reclaim `target/` from every idle worktree, landed or not: no live process in it
/// (`busy`), and nothing outside `target/` touched for `idle_secs` (`age`). Source is never
/// touched; the cost of a wrong guess is a rebuild. Oldest idle first. With `floor`
/// `Some((free, bytes))` it stops as soon as `free()` reports at least `bytes`; an
/// unreadable free-space figure reclaims nothing rather than everything.
pub fn reap_idle(
    worktrees: &Path,
    dry_run: bool,
    idle_secs: u64,
    age: &dyn Fn(&Path) -> Option<u64>,
    busy: &dyn Fn(&Path) -> bool,
    floor: Option<(&dyn Fn() -> Option<u64>, u64)>,
) -> Result<Reaped, String> {
    let mut out = Reaped::default();
    let mut idle: Vec<(u64, String, PathBuf, PathBuf)> = Vec::new();
    for (id, dir, t) in candidates(worktrees) {
        match age(&dir) {
            Some(a) if a >= idle_secs => {
                if busy(&dir) {
                    out.kept_busy += 1;
                } else {
                    idle.push((a, id, dir, t));
                }
            }
            Some(_) => out.kept_active += 1,
            None => out.kept_unknown.push(id),
        }
    }
    idle.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut freed = 0u64;
    for (_, id, _dir, t) in idle {
        if let Some((free, bytes)) = floor {
            match free() {
                Some(f) if dry_run && f + freed >= bytes => break,
                Some(f) if !dry_run && f >= bytes => break,
                Some(_) => {}
                None => break,
            }
        }
        let n = size_of(&t);
        if !dry_run {
            let r = if fs::symlink_metadata(&t).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                fs::remove_file(&t)
            } else {
                fs::remove_dir_all(&t)
            };
            if let Err(e) = r {
                return Err(format!("cannot remove {}: {e}", t.display()));
            }
        }
        freed += n;
        out.bytes += n;
        out.removed.push(id);
    }
    Ok(out)
}

/// Bytes available to this user on the filesystem holding `p`.
pub fn free_bytes(p: &Path) -> Option<u64> {
    let o = spira_config::bounded::bounded("df").args(["-Pk"]).arg(p).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&o.stdout);
    let kb: u64 = text.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()?;
    Some(kb * 1024)
}

/// The report line.
pub fn describe(r: &Reaped, dry_run: bool) -> String {
    let verb = if dry_run { "would remove" } else { "removed" };
    let mut s = format!(
        "target-reap: {verb} {} target dir(s), {} MiB{}; kept {} unlanded",
        r.removed.len(),
        r.bytes / (1024 * 1024),
        if r.removed.is_empty() { String::new() } else { format!(" ({})", r.removed.join(" ")) },
        r.kept_unlanded
    );
    if r.kept_busy > 0 {
        s.push_str(&format!("; kept {} busy", r.kept_busy));
    }
    if r.kept_active > 0 {
        s.push_str(&format!("; kept {} recently touched", r.kept_active));
    }
    if !r.kept_unknown.is_empty() {
        s.push_str(&format!("; kept {} cannot tell ({})", r.kept_unknown.len(), r.kept_unknown.join(" ")));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(testkit::TempDir, PathBuf);
    fn scratch(tag: &str) -> Scratch {
        let t = testkit::TempDir::new(&format!("target-reap-{tag}"));
        let p = t.path().to_path_buf();
        Scratch(t, p)
    }
    fn wt(root: &Path, name: &str, bytes: usize) {
        let d = root.join(name).join("target/aeon");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("x"), vec![1u8; bytes]).unwrap();
        fs::write(root.join(name).join("src.rs"), "kept").unwrap();
    }

    #[test]
    fn bead_ids_are_the_only_candidates() {
        for ok in ["sp-z61hj", "sp-zs04v.3", "sp-wenrl.2", "sp-n9z"] {
            assert!(is_bead_id(ok), "{ok}");
        }
        for no in ["round-122", ".gate.harness.x", "sp-s0e1k-shim", "sp-", "sp-ABC", "sp-a.", "sp-a.x", "batch-rust"] {
            assert!(!is_bead_id(no), "{no}");
        }
    }

    #[test]
    fn concierge_prefixed_dirs_resolve_to_the_bare_id_and_nothing_else_does() {
        assert_eq!(worktree_id("sp-x9kbg"), Some("sp-x9kbg"));
        assert_eq!(worktree_id("concierge-sp-x9kbg"), Some("sp-x9kbg"));
        assert_eq!(worktree_id("concierge-sp-zs04v.3"), Some("sp-zs04v.3"));
        for no in ["concierge-round-9", "concierge-", "round-9", "concierge-sp-s0e1k-shim"] {
            assert_eq!(worktree_id(no), None, "{no}");
        }
    }

    fn never_busy(_: &Path) -> bool {
        false
    }

    #[test]
    fn a_landed_worktrees_target_goes_whatever_the_bead_says_or_the_dir_is_named() {
        let s = scratch("reap");
        wt(&s.1, "sp-done", 8192);
        wt(&s.1, "concierge-sp-stuck", 2048); // sp-x9kbg: landed, bead never closes
        wt(&s.1, "sp-open", 4096); // unlanded: commits outstanding
        wt(&s.1, "sp-ghost", 4096); // landed-ness unknown
        wt(&s.1, "round-9", 4096); // not bead-named at all
        fs::create_dir_all(s.1.join("sp-notarget")).unwrap();
        let landed = |id: &str, _dir: &Path| -> Option<bool> {
            match id {
                "sp-done" | "sp-stuck" => Some(true),
                "sp-open" => Some(false),
                _ => None,
            }
        };
        let r = reap(&s.1, false, &landed, &never_busy).unwrap();
        let mut removed = r.removed.clone();
        removed.sort();
        assert_eq!(removed, vec!["sp-done".to_string(), "sp-stuck".to_string()], "landed goes, whatever the dir naming");
        assert!(r.bytes >= 8192 + 2048);
        assert_eq!((r.kept_unlanded, r.kept_unknown.clone()), (1, vec!["sp-ghost".to_string()]));
        assert!(!s.1.join("sp-done/target").exists() && s.1.join("sp-done/src.rs").is_file(), "only the build output goes");
        assert!(!s.1.join("concierge-sp-stuck/target").exists(), "landed, open bead or not, is still reaped");
        for kept in ["sp-open", "sp-ghost", "round-9"] {
            assert!(s.1.join(kept).join("target/aeon/x").is_file(), "{kept}");
        }
        let line = describe(&r, false);
        assert!(line.contains("kept 1 unlanded") && line.contains("sp-ghost"), "{line}");
    }

    #[test]
    fn sp_x9kbg_red_a_landed_branch_with_an_open_bead_in_a_concierge_dir_is_kept_today() {
        // THE REPORTED DEFECT (sp-x9kbg): a worktree whose content has landed still held
        // its target/ forever when (a) its bead stayed open, or (b) its directory was
        // named `concierge-<id>` rather than `<id>` — the old gate was bd status alone,
        // consulted only for bead-named directories. Pinned here as a regression check:
        // landed-ness alone decides now, so this reaps even with no bead answer at all.
        let s = scratch("red");
        wt(&s.1, "concierge-sp-open", 4096);
        let r = reap(&s.1, false, &|_, _| Some(true), &never_busy).unwrap();
        assert_eq!(r.removed, vec!["sp-open".to_string()], "a landed branch's target must go, whatever the bead's status or the dir's name");
    }

    #[test]
    fn unlanded_is_never_reaped_even_when_landed_ness_is_the_only_signal() {
        let s = scratch("unlanded");
        wt(&s.1, "sp-busy", 4096);
        let r = reap(&s.1, false, &|_, _| Some(false), &never_busy).unwrap();
        assert_eq!(r.removed, Vec::<String>::new());
        assert_eq!(r.kept_unlanded, 1);
        assert!(s.1.join("sp-busy/target/aeon/x").is_file());
    }

    #[test]
    fn unknown_landed_ness_reaps_nothing() {
        let s = scratch("unknown");
        wt(&s.1, "sp-done", 10);
        let r = reap(&s.1, false, &|_, _| None, &never_busy).unwrap();
        assert_eq!(r.removed, Vec::<String>::new());
        assert_eq!(r.kept_unknown, vec!["sp-done".to_string()]);
        assert!(s.1.join("sp-done/target/aeon/x").is_file());
    }

    #[test]
    fn dry_run_removes_nothing_and_says_so() {
        let s = scratch("dry");
        wt(&s.1, "sp-done", 10);
        let r = reap(&s.1, true, &|_, _| Some(true), &never_busy).unwrap();
        assert_eq!(r.removed, vec!["sp-done".to_string()]);
        assert!(s.1.join("sp-done/target/aeon/x").is_file());
        assert!(describe(&r, true).starts_with("target-reap: would remove 1"));
    }

    #[test]
    fn a_target_link_is_unlinked_never_followed() {
        let s = scratch("link");
        let elsewhere = s.1.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("keep"), "x").unwrap();
        fs::create_dir_all(s.1.join("wt/sp-done")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, s.1.join("wt/sp-done/target")).unwrap();
        let r = reap(&s.1.join("wt"), false, &|_, _| Some(true), &never_busy).unwrap();
        assert_eq!(r.removed.len(), 1);
        assert!(elsewhere.join("keep").is_file());
    }

    #[test]
    fn sp_x9kbg_a_busy_worktree_is_kept_even_though_it_is_landed() {
        // THE SECOND GUARD: landed-ness alone is not the last word — a live process still
        // holding the worktree open (crate::busy) keeps it, independent of what landed()
        // concluded. Defense in depth against exactly this bead's own two prior defects.
        let s = scratch("busy");
        wt(&s.1, "sp-inflight", 4096);
        let busy_dirs = std::cell::RefCell::new(Vec::new());
        let busy = |dir: &Path| {
            busy_dirs.borrow_mut().push(dir.to_path_buf());
            true
        };
        let r = reap(&s.1, false, &|_, _| Some(true), &busy).unwrap();
        assert_eq!(r.removed, Vec::<String>::new());
        assert_eq!(r.kept_busy, 1);
        assert!(s.1.join("sp-inflight/target/aeon/x").is_file(), "a busy worktree's target/ must survive even though it is landed");
        assert_eq!(busy_dirs.borrow().len(), 1);
    }

    fn age_from(map: &'static [(&'static str, u64)]) -> impl Fn(&Path) -> Option<u64> {
        move |d: &Path| {
            let n = d.file_name()?.to_str()?;
            map.iter().find(|(k, _)| *k == n).map(|(_, a)| *a)
        }
    }

    #[test]
    fn sp_blsz9_an_idle_worktrees_target_goes_landed_or_not_and_a_busy_or_recent_one_stays() {
        let s = scratch("idle");
        for n in ["sp-idle", "sp-recent", "sp-busy", "sp-nokey"] {
            wt(&s.1, n, 4096);
        }
        let age = age_from(&[("sp-idle", 7 * 3600), ("sp-recent", 60), ("sp-busy", 9 * 3600)]);
        let busy = |d: &Path| d.ends_with("sp-busy");
        let r = reap_idle(&s.1, false, 6 * 3600, &age, &busy, None).unwrap();
        assert_eq!(r.removed, vec!["sp-idle".to_string()]);
        assert_eq!((r.kept_active, r.kept_busy, r.kept_unknown.clone()), (1, 1, vec!["sp-nokey".to_string()]));
        assert!(!s.1.join("sp-idle/target").exists() && s.1.join("sp-idle/src.rs").is_file(), "source stays");
        for kept in ["sp-recent", "sp-busy", "sp-nokey"] {
            assert!(s.1.join(kept).join("target/aeon/x").is_file(), "{kept}");
        }
    }

    #[test]
    fn sp_blsz9_below_the_floor_reclaims_oldest_idle_first_and_stops_at_the_floor() {
        let s = scratch("floor");
        for n in ["sp-a", "sp-b", "sp-c"] {
            wt(&s.1, n, 4096);
        }
        let age = age_from(&[("sp-a", 10 * 3600), ("sp-b", 30 * 3600), ("sp-c", 20 * 3600)]);
        let root = s.1.clone();
        let free = move || Some(if root.join("sp-b/target").exists() { 10 } else { 100 });
        let r = reap_idle(&s.1, false, 6 * 3600, &age, &|_| false, Some((&free, 50))).unwrap();
        assert_eq!(r.removed, vec!["sp-b".to_string()], "the oldest, then stop once above the floor");
        assert!(s.1.join("sp-a/target").exists() && s.1.join("sp-c/target").exists());
        let none = || None;
        let r = reap_idle(&s.1, false, 6 * 3600, &age, &|_| false, Some((&none, 50))).unwrap();
        assert!(r.removed.is_empty(), "unreadable free space reclaims nothing");
    }

    #[test]
    fn idle_age_ignores_target_and_sees_source_changes() {
        let s = scratch("age");
        wt(&s.1, "sp-x", 10);
        let d = s.1.join("sp-x");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 3600);
        let set = |p: &Path| {
            fs::File::open(p).unwrap().set_modified(old).unwrap();
        };
        set(&d.join("src.rs"));
        set(&d);
        assert!(idle_age_secs(&d).unwrap() >= 10 * 3600, "fresh target/ files do not count");
        fs::write(d.join("src.rs"), "edit").unwrap();
        assert!(idle_age_secs(&d).unwrap() < 60, "an edit outside target/ does");
    }
}
