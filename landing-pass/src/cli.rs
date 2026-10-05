//! Argument parsing (DESIGN.md §2.1).

#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    /// `--pass` / `pr`: the pr-mode pass.
    Pr,
    /// `land`: the gated pass.
    Land,
    Halt { reason: Reason, dry_run: bool },
    /// `noverdict <id> <branch> <repo> <reason> <outcome>`: `spira_land_noverdict` alone,
    /// stdin = the gate output (sp-31hjr) — the real-sender suites' way to drive the
    /// native counting/escalation without a whole pass, the same shape as sentinel's
    /// `--land-escalate`.
    Noverdict { id: String, branch: String, repo: String, reason: String, outcome: String },
    /// `ask-rebase-loop <id> <branch> <repo> <n> <conflicts> <others> [<repo-dir> <base>]`:
    /// `spira_ask_rebase_loop` alone (sp-31hjr).
    AskRebaseLoop(Vec<String>),
    /// `land-subject <id>`: lib.sh `land_subject` alone (sp-81t4d).
    LandSubject { id: String },
    /// `pr-merged <repo> <branch>`: lib.sh `pr_merged` alone (sp-81t4d).
    PrMerged { repo: String, branch: String },
    /// `conflict-note <repo> <branch> <base> <name> <conflicts> <actor> [rq_n]`: lib.sh
    /// `conflict_reopen_note` alone (sp-81t4d). `rq_n` defaults to `1`.
    ConflictNote(Vec<String>),
    /// `other-beads <repo> <branch> <base> <files>`: lib.sh `other_beads_on_conflicts`
    /// alone (sp-81t4d).
    OtherBeads { repo: String, branch: String, base: String, files: String },
    /// `is-work-type <type>`: lib.sh `bead_is_work_type` alone (sp-81t4d). Exit 0 matches,
    /// 1 does not.
    IsWorkType { ty: String },
    Help,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reason {
    None,
    Text(String),
    /// `--reason-file F`, `-` for stdin.
    File(String),
}

pub const USAGE: &str = "usage: landing-pass --pass | land | halt [--reason T | --reason-file F|-] [--dry-run] | noverdict <id> <branch> <repo> <reason> <outcome> | land-subject <id> | pr-merged <repo> <branch> | conflict-note <repo> <branch> <base> <name> <conflicts> <actor> [rq_n] | other-beads <repo> <branch> <base> <files> | is-work-type <type>";

/// Err((exit code, message for stderr)).
pub fn parse(args: &[String]) -> Result<Cmd, (i32, String)> {
    match args.first().map(String::as_str) {
        Some("--pass") | Some("pr") if args.len() == 1 => Ok(Cmd::Pr),
        Some("land") if args.len() == 1 => Ok(Cmd::Land),
        Some("-h") | Some("--help") => Ok(Cmd::Help),
        Some("halt") => parse_halt(&args[1..]),
        Some("noverdict") if args.len() == 6 => Ok(Cmd::Noverdict {
            id: args[1].clone(),
            branch: args[2].clone(),
            repo: args[3].clone(),
            reason: args[4].clone(),
            outcome: args[5].clone(),
        }),
        Some("ask-rebase-loop") if args.len() == 7 || args.len() == 9 => Ok(Cmd::AskRebaseLoop(args[1..].to_vec())),
        Some("land-subject") if args.len() == 2 => Ok(Cmd::LandSubject { id: args[1].clone() }),
        Some("land-subject") => Err((2, "landing-pass land-subject: usage: land-subject <id>".to_string())),
        Some("pr-merged") if args.len() == 3 => Ok(Cmd::PrMerged { repo: args[1].clone(), branch: args[2].clone() }),
        Some("pr-merged") => Err((2, "landing-pass pr-merged: usage: pr-merged <repo> <branch>".to_string())),
        Some("conflict-note") if args.len() == 7 || args.len() == 8 => Ok(Cmd::ConflictNote(args[1..].to_vec())),
        Some("conflict-note") => {
            Err((2, "landing-pass conflict-note: usage: conflict-note <repo> <branch> <base> <name> <conflicts> <actor> [rq_n]".to_string()))
        }
        Some("other-beads") if args.len() == 5 => {
            Ok(Cmd::OtherBeads { repo: args[1].clone(), branch: args[2].clone(), base: args[3].clone(), files: args[4].clone() })
        }
        Some("other-beads") => Err((2, "landing-pass other-beads: usage: other-beads <repo> <branch> <base> <files>".to_string())),
        Some("is-work-type") if args.len() == 2 => Ok(Cmd::IsWorkType { ty: args[1].clone() }),
        Some("is-work-type") => Err((2, "landing-pass is-work-type: usage: is-work-type <type>".to_string())),
        _ => Err((2, USAGE.to_string())),
    }
}

fn parse_halt(a: &[String]) -> Result<Cmd, (i32, String)> {
    let mut reason = Reason::None;
    let mut dry_run = false;
    let mut i = 0;
    while i < a.len() {
        let x = a[i].as_str();
        match x {
            "--reason" | "--reason-file" => {
                let Some(v) = a.get(i + 1) else {
                    return Err((2, format!("landing halt: {x} requires an argument")));
                };
                reason = if x == "--reason" { Reason::Text(v.clone()) } else { Reason::File(v.clone()) };
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            _ if x.starts_with("--reason=") => {
                reason = Reason::Text(x["--reason=".len()..].to_string());
                i += 1;
            }
            _ if x.starts_with("--reason-file=") => {
                reason = Reason::File(x["--reason-file=".len()..].to_string());
                i += 1;
            }
            _ if x.starts_with("--") => return Err((2, format!("landing halt: unknown option: {x}"))),
            _ => return Err((2, format!("landing halt: unexpected argument: {x}"))),
        }
    }
    Ok(Cmd::Halt { reason, dry_run })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn verbs_and_halt_options() {
        assert_eq!(parse(&v(&["--pass"])), Ok(Cmd::Pr));
        assert_eq!(parse(&v(&["land"])), Ok(Cmd::Land));
        assert_eq!(
            parse(&v(&["halt", "--reason", "x y", "--dry-run"])),
            Ok(Cmd::Halt { reason: Reason::Text("x y".into()), dry_run: true })
        );
        assert_eq!(parse(&v(&["halt", "--reason=z"])), Ok(Cmd::Halt { reason: Reason::Text("z".into()), dry_run: false }));
        assert_eq!(parse(&v(&["halt", "--reason-file", "-"])), Ok(Cmd::Halt { reason: Reason::File("-".into()), dry_run: false }));
        assert_eq!(parse(&v(&["halt", "--reason"])).unwrap_err().0, 2);
        assert_eq!(parse(&v(&["halt", "--bogus"])).unwrap_err().1, "landing halt: unknown option: --bogus");
        assert_eq!(parse(&v(&["halt", "x"])).unwrap_err().1, "landing halt: unexpected argument: x");
        assert_eq!(parse(&v(&[])).unwrap_err().0, 2);
        assert_eq!(parse(&v(&["land", "extra"])).unwrap_err().0, 2);
        assert_eq!(
            parse(&v(&["noverdict", "sp-a", "spira/sp-a", "spira", "lock", "NO_VERDICT"])),
            Ok(Cmd::Noverdict {
                id: "sp-a".into(),
                branch: "spira/sp-a".into(),
                repo: "spira".into(),
                reason: "lock".into(),
                outcome: "NO_VERDICT".into(),
            })
        );
        assert_eq!(parse(&v(&["noverdict", "sp-a"])).unwrap_err().0, 2);
        assert_eq!(
            parse(&v(&["ask-rebase-loop", "sp-a", "spira/sp-a", "spira", "3", "foo.sh", ""])),
            Ok(Cmd::AskRebaseLoop(v(&["sp-a", "spira/sp-a", "spira", "3", "foo.sh", ""])))
        );
        assert_eq!(parse(&v(&["ask-rebase-loop", "sp-a"])).unwrap_err().0, 2);
    }

    #[test]
    fn the_landstate_and_landed_oracle_verbs_are_gone() {
        // sp-2c1n0: the landstate ledger and the subject/notes oracles are deleted; the
        // lifecycle record (spira-lc) is the one answer. Each name is now an unknown verb.
        for verb in ["mark", "state", "landed", "cited-commit", "close-on-land", "landstate"] {
            assert_eq!(parse(&v(&[verb, "sp-a", "x", "y"])).unwrap_err().0, 2, "{verb}");
            assert!(!USAGE.contains(&format!("| {verb} ")), "{verb} still in USAGE");
        }
    }

    #[test]
    fn family_r_verbs() {
        assert_eq!(parse(&v(&["land-subject", "sp-a"])), Ok(Cmd::LandSubject { id: "sp-a".into() }));
        assert_eq!(parse(&v(&["land-subject"])).unwrap_err().0, 2);
        assert_eq!(parse(&v(&["pr-merged", "/repo", "spira/sp-a"])), Ok(Cmd::PrMerged { repo: "/repo".into(), branch: "spira/sp-a".into() }));
        assert_eq!(parse(&v(&["pr-merged", "/repo"])).unwrap_err().0, 2);
        assert_eq!(
            parse(&v(&["conflict-note", "/repo", "spira/sp-a", "origin/main", "spira", "f.txt", "sentinel", "2"])),
            Ok(Cmd::ConflictNote(v(&["/repo", "spira/sp-a", "origin/main", "spira", "f.txt", "sentinel", "2"])))
        );
        assert_eq!(
            parse(&v(&["conflict-note", "/repo", "spira/sp-a", "origin/main", "spira", "f.txt", "sentinel"])),
            Ok(Cmd::ConflictNote(v(&["/repo", "spira/sp-a", "origin/main", "spira", "f.txt", "sentinel"])))
        );
        assert_eq!(parse(&v(&["conflict-note", "/repo"])).unwrap_err().0, 2);
        assert_eq!(
            parse(&v(&["other-beads", "/repo", "spira/sp-a", "origin/main", "f.txt"])),
            Ok(Cmd::OtherBeads { repo: "/repo".into(), branch: "spira/sp-a".into(), base: "origin/main".into(), files: "f.txt".into() })
        );
        assert_eq!(parse(&v(&["other-beads", "/repo"])).unwrap_err().0, 2);
        assert_eq!(parse(&v(&["is-work-type", "bug"])), Ok(Cmd::IsWorkType { ty: "bug".into() }));
        assert_eq!(parse(&v(&["is-work-type"])).unwrap_err().0, 2);
    }
}
