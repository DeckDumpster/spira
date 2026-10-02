//! `groomer unpoison <id> --cause <c> --evidence <text>` — lifts `spira-poison` when the
//! charge was the HARNESS's fault, not the work's. Delegates to `spira-claim unpoison
//! --credit <cause>` (spira-claim/DESIGN.md §8): the unjudged credit, the
//! `poison.cleared` floor, the lifecycle hold, the note and the ask are all spira-claim's,
//! verified by CHECK 4's own decision — this crate does not re-derive any of it.

use std::process::{Command, Stdio};

#[derive(Debug)]
pub struct Opts {
    pub id: String,
    pub cause: String,
    pub evidence: String,
}

/// Parse `<id> --cause <c> --evidence <text>`. Both flags are REQUIRED: a lift with no
/// evidence is indistinguishable from an ungrounded amnesty.
pub fn parse(id: &str, cause: Option<&str>, evidence: Option<&str>) -> Result<Opts, String> {
    if id.is_empty() {
        return Err("unpoison: bead id required".into());
    }
    let cause = cause.unwrap_or("").to_string();
    if cause.is_empty() {
        return Err("unpoison: --cause <c> is required (e.g. pre-session-death, branch-collision, yield-headless, gate-still-running, precondition-satisfied)".into());
    }
    let evidence = evidence.unwrap_or("").to_string();
    if evidence.is_empty() {
        return Err(
            "unpoison: --evidence <text> is required\ngroomer: a lift without evidence may be an ungrounded amnesty in disguise;\ngroomer: name the sessions and the harness fault this cause credits".into(),
        );
    }
    Ok(Opts { id: id.to_string(), cause, evidence })
}

/// The exact `spira-claim unpoison` invocation `groomer.sh` made.
pub fn spira_claim_args(o: &Opts) -> Vec<String> {
    vec![
        "unpoison".into(),
        "--bead".into(),
        o.id.clone(),
        "--cause".into(),
        format!("{}: {}", o.cause, o.evidence),
        "--credit".into(),
        o.cause.clone(),
        "--actor".into(),
        "groomer".into(),
    ]
}

/// Run `spira-claim unpoison …`, print its output, and judge the same prefix
/// `groomer.sh` checked: `OK   <id>:`.
pub fn run(bin: &str, o: &Opts) -> (i32, String) {
    let args = spira_claim_args(o);
    let out = Command::new(bin).args(&args).stdin(Stdio::null()).output();
    let out = match out {
        Ok(o) => o,
        Err(e) => return (1, format!("groomer: unpoison: could not run {bin}: {e}")),
    };
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    if text.starts_with(&format!("OK   {}:", o.id)) {
        (0, format!("{text}UNPOISONED {} cause={}\n", o.id, o.cause))
    } else {
        (1, format!("{text}groomer: unpoison: {} was not cleared (rc={}) — see above\n", o.id, out.status.code().unwrap_or(-1)))
    }
}

#[cfg(test)]
mod unpoison_tests {
    use super::*;

    #[test]
    fn refuses_without_cause() {
        let err = parse("sp-1", None, Some("evidence")).unwrap_err();
        assert!(err.contains("--cause"));
    }

    #[test]
    fn refuses_without_evidence() {
        let err = parse("sp-1", Some("branch-collision"), None).unwrap_err();
        assert!(err.contains("--evidence"));
    }

    #[test]
    fn refuses_without_an_id() {
        assert!(parse("", Some("c"), Some("e")).is_err());
    }

    #[test]
    fn builds_the_expected_spira_claim_invocation() {
        let o = parse("sp-1", Some("branch-collision"), Some("worktree collided with sp-2")).unwrap();
        let args = spira_claim_args(&o);
        assert_eq!(
            args,
            vec![
                "unpoison", "--bead", "sp-1", "--cause", "branch-collision: worktree collided with sp-2", "--credit", "branch-collision", "--actor", "groomer",
            ]
        );
    }

    #[test]
    fn judges_a_non_matching_output_as_failure() {
        let o = parse("sp-1", Some("c"), Some("e")).unwrap();
        // echo prints its argv, which does not start with "OK   sp-1:", so this exercises
        // the failure branch — see the next test for the success branch via a stub.
        let (rc, out) = run("echo", &o);
        assert_eq!(rc, 1);
        assert!(out.contains("was not cleared"));
    }

    #[test]
    fn judges_an_ok_prefix_from_the_real_binary_as_success() {
        let dir = testkit::TempDir::new("groomer-unpoison");
        let stub = dir.path().join("spira-claim");
        // testkit::write_exe, never fs::write + set_permissions: this process's own
        // write-fd would otherwise sit open for the brief window between the two calls,
        // and a concurrent fork elsewhere in this test binary that forks in that window
        // inherits a duplicate of it — the kernel then refuses to exec the file
        // (ETXTBSY) until that unrelated child closes it or execs (sp-os3of, the third
        // occurrence after sp-xtdqi-3's spira-config and doctor).
        testkit::write_exe(&stub, "#!/usr/bin/env bash\nprintf 'OK   sp-1: cleared — attempts 3 -> 0\\n'\n");
        let o = parse("sp-1", Some("branch-collision"), Some("worktree collision")).unwrap();
        let (rc, out) = run(stub.to_str().unwrap(), &o);
        assert_eq!(rc, 0, "output was: {out}");
        assert!(out.contains("UNPOISONED sp-1 cause=branch-collision"));
    }
}
