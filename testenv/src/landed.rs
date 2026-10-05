//! Whether a worktree's bead has landed (sp-x9kbg, sp-2c1n0) — the one question `target-reap`
//! ([`crate::reap`]) asks before freeing a `target/`. The answer is the lifecycle record's
//! alone: `spira-lc state <id>` reads LANDED. Never bd's status (a bead that stays open past
//! landing used to hold its target/ hostage, sp-x9kbg) and never a search of the base's
//! commit subjects (the retired landed oracle, deleted by sp-2c1n0). A fresh, just-claimed
//! worktree is WORKING on the record, so its tip trivially equalling the base it was cut
//! from (round 198's reaped aeons) can no longer read as landed.

use std::process::{Command, Stdio};

/// spira-lc's own exit for "the record holds no row" (callers.rs NO_ROW).
const NO_ROW: i32 = 1;

/// The answer `spira-lc state <id>` gave: exit 0 with the state, NO_ROW, or anything else.
/// `Some(true)` LANDED; `Some(false)` any other state, or no row at all (nothing says it
/// landed); `None` the record could not answer — fail closed, the worktree is kept.
pub fn answer(code: Option<i32>, stdout: &str) -> Option<bool> {
    match code {
        Some(0) => Some(stdout.trim() == "LANDED"),
        Some(NO_ROW) => Some(false),
        _ => None,
    }
}

/// `lc_bin state <id>`, bounded at 5 s (call-deadline), with `env` laid over this process's
/// own environment (the release's PATH, so the release's own spira-lc answers first).
pub fn lc_landed(lc_bin: &str, env: &[(String, String)], id: &str) -> Option<bool> {
    let o = Command::new("timeout")
        .args(["5", lc_bin, "state", id])
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    answer(o.status.code(), &String::from_utf8_lossy(&o.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_landed_row_is_landed_and_an_unanswerable_record_is_unknown() {
        assert_eq!(answer(Some(0), "LANDED\n"), Some(true));
        assert_eq!(answer(Some(0), "WORKING\n"), Some(false), "a fresh claim is never landed");
        assert_eq!(answer(Some(0), "CERTIFIED"), Some(false));
        assert_eq!(answer(Some(NO_ROW), ""), Some(false), "no row: nothing says it landed");
        assert_eq!(answer(Some(2), ""), None, "cannot tell");
        assert_eq!(answer(None, ""), None, "killed by the deadline");
    }

    #[test]
    fn the_record_is_asked_through_the_named_binary() {
        let t = testkit::TempDir::new("testenv-lc-landed");
        let bin = t.join("spira-lc");
        testkit::write_exe(
            &bin,
            "#!/bin/sh\n[ \"$1\" = state ] || exit 2\ncase \"$2\" in sp-l) echo LANDED ;; sp-w) echo WORKING ;; sp-n) exit 1 ;; *) exit 2 ;; esac\n",
        );
        let b = bin.to_string_lossy();
        assert_eq!(lc_landed(&b, &[], "sp-l"), Some(true));
        assert_eq!(lc_landed(&b, &[], "sp-w"), Some(false));
        assert_eq!(lc_landed(&b, &[], "sp-n"), Some(false));
        assert_eq!(lc_landed(&b, &[], "sp-x"), None);
    }

    #[test]
    fn no_commit_subject_search_survives() {
        // sp-2c1n0 guard: the subject oracle (`git log --grep` for `spira: land <id>`) is gone.
        let src = include_str!("landed.rs");
        let needle = ["\"log\"", "\"--grep\""].join(", ");
        assert!(!src.contains(&needle), "a commit-subject landed search came back");
    }
}
