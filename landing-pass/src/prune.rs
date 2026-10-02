//! The landstate prune (DESIGN.md §5). It bounds `landstate/` for beads no reader will look
//! up again — and it NEVER deletes a LANDED record whose tip is not yet on the forge: the
//! record is the tip publish prefers and the drain watchtower meters, so it must outlive its
//! publication, not its branch (sp-bauwt; absorbs the landstate-keep-unpublished override).

use crate::model::LandState;
use crate::pass::Pass;
use std::collections::HashMap;
use std::fs;

/// Whether a record may go, given its bead's raw status and whether any repository still
/// has its branch. `on_forge` is asked only for a LANDED record.
pub fn may_prune(raw_status: Option<&str>, has_branch: bool, ls: Option<&LandState>, on_forge: &dyn Fn(&str) -> bool) -> bool {
    if raw_status != Some("closed") || has_branch {
        return false;
    }
    match ls {
        Some(l) if l.state == "LANDED" => !l.tip.is_empty() && l.tip != "none" && on_forge(&l.tip),
        _ => true,
    }
}

pub fn prune_landstate(p: &Pass) {
    let dir = p.files.landstate_dir();
    let Ok(rd) = fs::read_dir(&dir) else { return };
    let mut ids: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.contains('.'))
        .collect();
    ids.sort();
    if ids.is_empty() {
        return;
    }
    // One bulk read; an unreadable store keeps everything.
    let status: HashMap<String, String> = match p.beads.show(&ids) {
        Ok(rows) => rows.into_iter().map(|b| (b.id, b.raw_status)).collect(),
        Err(_) => return,
    };
    let checkouts: Vec<_> = p.repos.iter().filter(|r| !r.path.as_os_str().is_empty() && r.path.join(".git").exists()).collect();
    // law-absence-needs-a-positive-control (sp-8bhnr, LOOP-STOPPING): "no repository could
    // be looked at" and "no repository has the branch" read identically to the loop below
    // (`checkouts.iter().any(...)` is false either way), and a registry that resolved every
    // configured repository to something other than a real checkout (sp-8bhnr's own repro:
    // the home repo 'spira' resolved to a release directory, not its configured checkout)
    // used to fall straight through as "has_branch = false for every bead" — pruning the
    // landstate of every closed bead in the store, sight unseen. Refuse outright, loudly,
    // whenever this pass could not actually look: any configured repository that did not
    // resolve to a real checkout, or none did at all (no repositories configured reads the
    // same way — nothing was checked, so nothing may be concluded from it).
    if p.repos.is_empty() || checkouts.len() != p.repos.len() {
        let missing: Vec<&str> = p.repos.iter().filter(|r| !checkouts.iter().any(|c| c.name == r.name)).map(|r| r.name.as_str()).collect();
        p.out.log(&format!(
            "landing: prune refused — {}/{} configured repositor{} resolved as a git checkout this pass ({}); a closed bead with no branch there may simply be unreadable, not actually landed. Pruning nothing.",
            checkouts.len(),
            p.repos.len(),
            if p.repos.len() == 1 { "y" } else { "ies" },
            if missing.is_empty() { "none configured".to_string() } else { missing.join(", ") }
        ));
        return;
    }
    for id in ids {
        let raw = status.get(&id).map(String::as_str);
        if raw != Some("closed") {
            continue;
        }
        let branch = format!("spira/{id}");
        let has_branch = checkouts.iter().any(|r| p.git.branch_exists(&r.path, &branch));
        let ls = p.files.land_state(&id);
        let on_forge = |tip: &str| {
            checkouts.iter().any(|r| match &r.forge_ref {
                Some(f) => p.git.rev_parse(&r.path, &format!("{tip}^{{commit}}")).is_some() && p.git.is_ancestor(&r.path, tip, f),
                None => false,
            })
        };
        if !may_prune(raw, has_branch, ls.as_ref(), &on_forge) {
            continue;
        }
        let st = ls.map(|l| l.state).unwrap_or_else(|| "?".into());
        p.out.log(&format!("landing: pruning landstate/{id} — closed bead with no branch (state was {st})"));
        let _ = fs::remove_file(dir.join(&id));
        let _ = fs::remove_file(dir.join(format!("{id}.ejected")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ls(state: &str, tip: &str) -> LandState {
        LandState { state: state.into(), tip: tip.into(), at: 1, reason: String::new() }
    }

    #[test]
    fn a_landed_record_waits_for_the_forge() {
        let never = |_: &str| false;
        let always = |_: &str| true;
        assert!(!may_prune(Some("closed"), false, Some(&ls("LANDED", "abc")), &never));
        assert!(may_prune(Some("closed"), false, Some(&ls("LANDED", "abc")), &always));
        assert!(!may_prune(Some("closed"), false, Some(&ls("LANDED", "none")), &always));
    }

    #[test]
    fn open_unknown_or_branched_beads_keep_their_record() {
        let always = |_: &str| true;
        assert!(!may_prune(Some("open"), false, Some(&ls("RED", "a")), &always));
        assert!(!may_prune(None, false, Some(&ls("RED", "a")), &always));
        assert!(!may_prune(Some("closed"), true, Some(&ls("CONTENT", "a")), &always));
        assert!(may_prune(Some("closed"), false, Some(&ls("CONTENT", "a")), &always));
        assert!(may_prune(Some("closed"), false, None, &always));
    }
}
