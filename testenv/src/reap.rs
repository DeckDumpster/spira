//! `target-reap` (sp-z61hj; spira-config/DESIGN-build-cache.md §2.4): remove the `target/`
//! of a worktree whose bead is closed. Build output of finished work is the largest thing on
//! the box's disk (159 dirs, 236 GiB measured 2026-09-30) and nothing reads it again.
//!
//! Candidates are `<worktrees>/<bead id>/target` only — the aeon and Concierge convention.
//! Gate trees and testenv slots are owned by the gate and testenv. One `bd show <ids…>
//! --json` decides: `closed` is reaped; open, unknown, or an unreadable answer is kept (fail
//! closed — never guess that work is finished).

use std::collections::BTreeMap;
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

/// `(bead id, <dir>/target)` for every bead-named worktree under `worktrees` that holds a
/// `target` (a directory or a link), sorted by id.
pub fn candidates(worktrees: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = fs::read_dir(worktrees)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let t = e.path().join("target");
                    (is_bead_id(&name) && e.path().is_dir() && fs::symlink_metadata(&t).is_ok()).then_some((name, t))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// `id → status` from `bd show … --json` output (an array of objects). Err when the text is
/// not that shape: nothing is reaped on an answer that cannot be read.
pub fn statuses(json: &str) -> Result<BTreeMap<String, String>, String> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).map_err(|e| format!("bd show --json: {e}"))?;
    let arr = v.as_array().ok_or("bd show --json: not an array")?;
    let mut m = BTreeMap::new();
    for o in arr {
        if let (Some(id), Some(st)) = (o.get("id").and_then(|x| x.as_str()), o.get("status").and_then(|x| x.as_str())) {
            m.insert(id.to_string(), st.to_string());
        }
    }
    Ok(m)
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
    pub kept_open: usize,
    pub kept_unknown: Vec<String>,
}

/// Reap: `show` answers `bd show <ids> --json` (Err = unreadable → nothing is removed).
pub fn reap(worktrees: &Path, dry_run: bool, show: &dyn Fn(&[String]) -> Result<String, String>) -> Result<Reaped, String> {
    let cands = candidates(worktrees);
    let mut out = Reaped::default();
    if cands.is_empty() {
        return Ok(out);
    }
    let ids: Vec<String> = cands.iter().map(|(i, _)| i.clone()).collect();
    let st = statuses(&show(&ids)?)?;
    for (id, t) in cands {
        match st.get(&id).map(String::as_str) {
            Some("closed") => {
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
            Some(_) => out.kept_open += 1,
            None => out.kept_unknown.push(id),
        }
    }
    Ok(out)
}

/// The report line.
pub fn describe(r: &Reaped, dry_run: bool) -> String {
    let verb = if dry_run { "would remove" } else { "removed" };
    let mut s = format!(
        "target-reap: {verb} {} target dir(s), {} MiB{}; kept {} of open beads",
        r.removed.len(),
        r.bytes / (1024 * 1024),
        if r.removed.is_empty() { String::new() } else { format!(" ({})", r.removed.join(" ")) },
        r.kept_open
    );
    if !r.kept_unknown.is_empty() {
        s.push_str(&format!("; kept {} bd does not know ({})", r.kept_unknown.len(), r.kept_unknown.join(" ")));
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
    fn a_closed_beads_target_goes_and_everything_else_stays() {
        let s = scratch("reap");
        wt(&s.1, "sp-done", 8192);
        wt(&s.1, "sp-open", 4096);
        wt(&s.1, "sp-ghost", 4096);
        wt(&s.1, "round-9", 4096);
        fs::create_dir_all(s.1.join("sp-notarget")).unwrap();
        let asked = std::cell::RefCell::new(Vec::new());
        let show = |ids: &[String]| {
            asked.borrow_mut().extend(ids.iter().cloned());
            Ok(r#"[{"id":"sp-done","status":"closed"},{"id":"sp-open","status":"in_progress"}]"#.to_string())
        };
        let r = reap(&s.1, false, &show).unwrap();
        assert_eq!(asked.borrow().as_slice(), &["sp-done", "sp-ghost", "sp-open"], "one bd call, bead-named worktrees with a target only");
        assert_eq!(r.removed, vec!["sp-done".to_string()]);
        assert!(r.bytes >= 8192);
        assert_eq!((r.kept_open, r.kept_unknown.clone()), (1, vec!["sp-ghost".to_string()]));
        assert!(!s.1.join("sp-done/target").exists() && s.1.join("sp-done/src.rs").is_file(), "only the build output goes");
        for kept in ["sp-open", "sp-ghost", "round-9"] {
            assert!(s.1.join(kept).join("target/aeon/x").is_file(), "{kept}");
        }
        let line = describe(&r, false);
        assert!(line.starts_with("target-reap: removed 1 target dir(s)") && line.contains("sp-ghost"), "{line}");
    }

    #[test]
    fn an_unreadable_answer_reaps_nothing() {
        let s = scratch("fail");
        wt(&s.1, "sp-done", 10);
        assert!(reap(&s.1, false, &|_| Err("bd show failed: timeout".into())).is_err());
        assert!(reap(&s.1, false, &|_| Ok("Error: no database".into())).is_err());
        assert!(s.1.join("sp-done/target/aeon/x").is_file());
    }

    #[test]
    fn dry_run_removes_nothing_and_says_so() {
        let s = scratch("dry");
        wt(&s.1, "sp-done", 10);
        let r = reap(&s.1, true, &|_| Ok(r#"[{"id":"sp-done","status":"closed"}]"#.into())).unwrap();
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
        let r = reap(&s.1.join("wt"), false, &|_| Ok(r#"[{"id":"sp-done","status":"closed"}]"#.into())).unwrap();
        assert_eq!(r.removed.len(), 1);
        assert!(elsewhere.join("keep").is_file());
    }
}
