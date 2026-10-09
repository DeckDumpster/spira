//! Argument parsing (DESIGN.md §2.1). Errors are usage errors: exit 2 with the message.

use std::path::PathBuf;

/// Where a text or member list comes from: argv (legacy callers) or a file / stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Text {
    None,
    Arg(String),
    File(PathBuf),
    Stdin,
}

impl Text {
    pub fn is_none(&self) -> bool {
        matches!(self, Text::None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    Submit { branch: String, repo: Option<String> },
    Protect { repo: Option<String> },
    Stats,
    Flush { repo: Option<String> },
    Step { repo: String },
    /// `verdict <repo>`: settle the repository's CI result (DESIGN-verdict.md).
    Verdict { repo: String },
    /// `step --all`: every queue-mode repository (queue-step-all.md).
    StepAll,
    Eject { id: String, repo: Option<String>, reason: Text, suites: String, red: bool, harness_fault: bool, dry_run: bool },
    Abandon { repo: Option<String>, reason: Text, dry_run: bool },
    OpenBatch { repo: Option<String>, members: Text, skip_pregate: bool, dry_run: bool },
    Claim { repo: Option<String>, reason: Text, force: bool },
    Release { repo: Option<String> },
    LandLocal { repo: Option<String>, head: String, members: Text, worktree: Option<PathBuf> },
    Publish { repo: Option<String> },
    PublishSettle { repo: Option<String> },
    ToForge { repo: Option<String> },
    ToLocal { repo: Option<String> },
    RollbackLocal { repo: Option<String> },
    Round(Round),
    Help,
}

/// `round <verb>`: the shared round lifecycle (ops/round.rs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Round {
    Open { repo: Option<String>, members: Text, name: Option<String>, worktree: Option<PathBuf> },
    Certify { batch: String, repo: Option<String>, attest: Option<String> },
    Eject { batch: String, id: String, repo: Option<String>, reason: Text, suites: String, red: bool, harness_fault: bool, rebuild: bool },
    Land { batch: String, repo: Option<String> },
    Abandon { batch: String, repo: Option<String>, reason: Text },
    Preempt { batch: String, repo: Option<String>, eject: String, reason: Text, suites: String },
    PassStart { batch: String, repo: Option<String> },
    SuitesStarted { batch: String, repo: Option<String> },
    PassVerdict { batch: String, repo: Option<String>, verdict: String, red_suites: String, suites_s: u64, build_s: u64, reason: Text },
    Status { repo: Option<String> },
    /// Assemble the next round behind the open one and run the fences on it.
    Stage { repo: Option<String>, members: Text, name: Option<String>, worktree: Option<PathBuf> },
    /// The staged round's own VM pass, once the round it waits behind is green.
    StageTest { repo: Option<String> },
    /// The round it waited behind landed: cut the staged round, reusing its pass when the tree matches.
    Promote { repo: Option<String> },
    Discard { repo: Option<String>, reason: Text },
}

pub const USAGE: &str = "usage: queue.sh submit <branch> [<repo>] | queue.sh protect [<repo>] | queue.sh stats | queue.sh flush [<repo>] | queue.sh step <repo> | queue.sh step --all | queue.sh verdict <repo> | queue.sh eject <id> [--reason <text>] [--harness-fault] [--dry-run] [<repo>] | queue.sh abandon [<repo>] --reason <text> [--dry-run] | queue.sh open-batch [<repo>] [--members <ids>] [--skip-pregate] [--dry-run] | queue.sh claim [<repo>] --reason <text> [--force] | queue.sh release [<repo>] | queue.sh land-local [<repo>] --head <sha> --members <id:tip[,id:tip...]> | queue.sh publish [<repo>] | queue.sh publish-settle [<repo>] | queue.sh to-forge [<repo>] | queue.sh to-local [<repo>] | queue.sh rollback-local [<repo>] | queue.sh round open [<repo>] --members <id[:tip],...> [--name <n>] [--worktree <dir>] | queue.sh round certify <batch> [<repo>] [--attest <head>] | queue.sh round eject <batch> <id> [<repo>] --reason <text> [--suites <csv>] [--red] [--harness-fault] [--no-rebuild] | queue.sh round land <batch> [<repo>] | queue.sh round abandon <batch> [<repo>] --reason <text> | queue.sh round preempt <batch> [<repo>] --eject <id>[,<id>...] --reason <text> [--suites <csv>] | queue.sh round pass-start <batch> [<repo>] | queue.sh round suites-started <batch> [<repo>] | queue.sh round pass-verdict <batch> [<repo>] --verdict <green|red|incomplete> [--red-suites <csv>] [--suites-s <n>] [--build-s <n>] [--reason <text>] | queue.sh round status [<repo>] | queue.sh round stage [<repo>] --members <id[:tip],...> [--name <n>] [--worktree <dir>] | queue.sh round stage-test [<repo>] | queue.sh round promote [<repo>] | queue.sh round discard [<repo>] --reason <text>";

/// A usage error: the message queue.sh printed (without trailing newline) and exit 2.
#[derive(Debug, PartialEq, Eq)]
pub struct Usage(pub String);

/// One option-parsing walk: `--k v` and `--k=v`, flags, and positionals.
struct Walk<'a> {
    cmd: &'a str,
    args: &'a [String],
    i: usize,
}

enum Tok<'a> {
    Opt(&'a str, Option<String>),
    Pos(&'a str),
}

impl<'a> Walk<'a> {
    fn next(&mut self) -> Option<Tok<'a>> {
        let a = self.args.get(self.i)?;
        self.i += 1;
        if let Some(rest) = a.strip_prefix("--") {
            if let Some((k, v)) = rest.split_once('=') {
                return Some(Tok::Opt(k, Some(v.to_string())));
            }
            return Some(Tok::Opt(rest, None));
        }
        if a.starts_with('-') && a.len() > 1 {
            return Some(Tok::Opt(a.as_str(), None));
        }
        Some(Tok::Pos(a.as_str()))
    }

    /// The value of an option: inline (`--k=v`) or the next argument (empty if none, as
    /// `shift; x="${1:-}"` did).
    fn value(&mut self, inline: Option<String>) -> String {
        if let Some(v) = inline {
            return v;
        }
        let v = self.args.get(self.i).cloned().unwrap_or_default();
        self.i += 1;
        v
    }

    fn unknown(&self, name: &str) -> Usage {
        let shown = if name.starts_with('-') { name.to_string() } else { format!("--{name}") };
        Usage(format!("queue.sh {}: unknown option: {shown}", self.cmd))
    }
}

fn text_file(v: String) -> Text {
    if v == "-" {
        Text::Stdin
    } else {
        Text::File(PathBuf::from(v))
    }
}

fn reason_opt(w: &mut Walk, k: &str, inline: Option<String>, reason: &mut Text) -> bool {
    match k {
        "reason" => {
            *reason = Text::Arg(w.value(inline));
            true
        }
        "reason-file" => {
            *reason = text_file(w.value(inline));
            true
        }
        _ => false,
    }
}

fn members_opt(w: &mut Walk, k: &str, inline: Option<String>, members: &mut Text) -> bool {
    match k {
        "members" => {
            *members = Text::Arg(w.value(inline));
            true
        }
        "members-file" => {
            *members = text_file(w.value(inline));
            true
        }
        _ => false,
    }
}

/// Subcommands whose only argument is an optional repo, refusing any option.
fn repo_only(cmd: &str, args: &[String]) -> Result<Option<String>, Usage> {
    let mut w = Walk { cmd, args, i: 0 };
    let mut repo = None;
    while let Some(t) = w.next() {
        match t {
            Tok::Opt(k, _) => return Err(w.unknown(k)),
            Tok::Pos(p) => repo = Some(p.to_string()),
        }
    }
    Ok(repo)
}

fn parse_round(args: &[String]) -> Result<Round, Usage> {
    let Some(verb) = args.first() else { return Err(Usage(USAGE.into())) };
    let label = format!("round {verb}");
    let mut w = Walk { cmd: &label, args: &args[1..], i: 0 };
    let (mut members, mut reason) = (Text::None, Text::None);
    let (mut name, mut worktree, mut attest) = (None, None, None);
    let (mut suites, mut red, mut harness_fault, mut rebuild) = (String::new(), false, false, true);
    let mut eject = String::new();
    let (mut verdict, mut red_suites, mut suites_s, mut build_s) = (String::new(), String::new(), None, None);
    let mut pos: Vec<String> = Vec::new();
    while let Some(t) = w.next() {
        match t {
            Tok::Opt(k, v) if members_opt(&mut w, k, v.clone(), &mut members) => {}
            Tok::Opt(k, v) if reason_opt(&mut w, k, v.clone(), &mut reason) => {}
            Tok::Opt("name", v) => name = Some(w.value(v)),
            Tok::Opt("worktree", v) => worktree = Some(PathBuf::from(w.value(v))),
            Tok::Opt("attest", v) => attest = Some(w.value(v)),
            Tok::Opt("suites", v) => suites = w.value(v),
            Tok::Opt("eject", v) => eject = w.value(v),
            Tok::Opt("verdict", v) => verdict = w.value(v),
            Tok::Opt("red-suites", v) => red_suites = w.value(v),
            Tok::Opt("suites-s", v) => suites_s = Some(w.value(v)),
            Tok::Opt("build-s", v) => build_s = Some(w.value(v)),
            Tok::Opt("red", None) => red = true,
            Tok::Opt("harness-fault", None) => harness_fault = true,
            Tok::Opt("no-rebuild", None) => rebuild = false,
            Tok::Opt(k, _) => return Err(w.unknown(k)),
            Tok::Pos(p) => pos.push(p.to_string()),
        }
    }
    let need = |what: &str, v: Option<String>| v.ok_or_else(|| Usage(format!("queue.sh round {verb}: {what} required")));
    let mut pos = pos.into_iter();
    match verb.as_str() {
        "open" => {
            if matches!(&members, Text::None) {
                return Err(Usage("queue.sh round open: --members is required".into()));
            }
            Ok(Round::Open { repo: pos.next(), members, name, worktree })
        }
        "certify" => Ok(Round::Certify { batch: need("batch id", pos.next())?, repo: pos.next(), attest }),
        "eject" => Ok(Round::Eject { batch: need("batch id", pos.next())?, id: need("bead id", pos.next())?, repo: pos.next(), reason, suites, red, harness_fault, rebuild }),
        "land" => Ok(Round::Land { batch: need("batch id", pos.next())?, repo: pos.next() }),
        "abandon" => Ok(Round::Abandon { batch: need("batch id", pos.next())?, repo: pos.next(), reason }),
        "preempt" => {
            if eject.split(',').all(str::is_empty) {
                return Err(Usage("queue.sh round preempt: --eject <id>[,<id>...] is required".into()));
            }
            Ok(Round::Preempt { batch: need("batch id", pos.next())?, repo: pos.next(), eject, reason, suites })
        }
        "pass-start" => Ok(Round::PassStart { batch: need("batch id", pos.next())?, repo: pos.next() }),
        "suites-started" => Ok(Round::SuitesStarted { batch: need("batch id", pos.next())?, repo: pos.next() }),
        "pass-verdict" => {
            if !["green", "red", "incomplete"].contains(&verdict.as_str()) {
                return Err(Usage("queue.sh round pass-verdict: --verdict green|red|incomplete is required".into()));
            }
            let secs = |what: &str, v: Option<String>| match v {
                None => Ok(0),
                Some(s) => s.parse::<u64>().map_err(|_| Usage(format!("queue.sh round pass-verdict: {what} must be a whole number of seconds"))),
            };
            Ok(Round::PassVerdict {
                batch: need("batch id", pos.next())?,
                repo: pos.next(),
                verdict,
                red_suites,
                suites_s: secs("--suites-s", suites_s)?,
                build_s: secs("--build-s", build_s)?,
                reason,
            })
        }
        "status" => Ok(Round::Status { repo: pos.next() }),
        "stage" => {
            if matches!(&members, Text::None) {
                return Err(Usage("queue.sh round stage: --members is required".into()));
            }
            Ok(Round::Stage { repo: pos.next(), members, name, worktree })
        }
        "stage-test" => Ok(Round::StageTest { repo: pos.next() }),
        "promote" => Ok(Round::Promote { repo: pos.next() }),
        "discard" => Ok(Round::Discard { repo: pos.next(), reason }),
        _ => Err(Usage(USAGE.into())),
    }
}

pub fn parse(argv: &[String]) -> Result<Cmd, Usage> {
    let Some(sub) = argv.first() else { return Err(Usage(USAGE.into())) };
    let args = &argv[1..];
    let pos = |i: usize| args.get(i).filter(|s| !s.is_empty()).cloned();
    match sub.as_str() {
        "-h" | "--help" | "help" => Ok(Cmd::Help),
        "submit" => {
            let branch = pos(0).ok_or_else(|| Usage("queue.sh submit: branch name required".into()))?;
            Ok(Cmd::Submit { branch, repo: pos(1) })
        }
        // protect/flush/step took their first positional and ignored the rest.
        "protect" => Ok(Cmd::Protect { repo: pos(0) }),
        "stats" => Ok(Cmd::Stats),
        "flush" => Ok(Cmd::Flush { repo: pos(0) }),
        "step" => {
            let repo = pos(0).ok_or_else(|| Usage("queue.sh step: repo required".into()))?;
            if repo == "--all" {
                if args.len() > 1 {
                    return Err(Usage("queue.sh step: --all takes no repository".into()));
                }
                return Ok(Cmd::StepAll);
            }
            if repo.starts_with("--") {
                return Err(Usage(format!("queue.sh step: unknown option: {repo}")));
            }
            Ok(Cmd::Step { repo })
        }
        "verdict" => {
            let repo = pos(0).ok_or_else(|| Usage("queue.sh verdict: repo required".into()))?;
            if repo.starts_with('-') || args.len() > 1 {
                return Err(Usage("queue.sh verdict: takes exactly one repository".into()));
            }
            Ok(Cmd::Verdict { repo })
        }
        "eject" => {
            let id = pos(0).ok_or_else(|| Usage("queue.sh eject: bead id required".into()))?;
            let mut w = Walk { cmd: "eject", args: &args[1..], i: 0 };
            let (mut repo, mut reason, mut suites, mut red, mut harness_fault, mut dry_run) = (None, Text::None, String::new(), false, false, false);
            while let Some(t) = w.next() {
                match t {
                    Tok::Opt(k, v) if reason_opt(&mut w, k, v.clone(), &mut reason) => {}
                    Tok::Opt("suites", v) => suites = w.value(v),
                    Tok::Opt("red", None) => red = true,
                    Tok::Opt("harness-fault", None) => harness_fault = true,
                    Tok::Opt("dry-run", None) => dry_run = true,
                    Tok::Opt(k, _) => return Err(w.unknown(k)),
                    Tok::Pos(p) => repo = Some(p.to_string()),
                }
            }
            Ok(Cmd::Eject { id, repo, reason, suites, red, harness_fault, dry_run })
        }
        "abandon" => {
            let mut w = Walk { cmd: "abandon", args, i: 0 };
            let (mut repo, mut reason, mut dry_run) = (None, Text::None, false);
            while let Some(t) = w.next() {
                match t {
                    Tok::Opt(k, v) if reason_opt(&mut w, k, v.clone(), &mut reason) => {}
                    Tok::Opt("dry-run", None) => dry_run = true,
                    Tok::Opt(k, _) => return Err(w.unknown(k)),
                    Tok::Pos(p) => repo = Some(p.to_string()),
                }
            }
            Ok(Cmd::Abandon { repo, reason, dry_run })
        }
        "open-batch" => {
            let mut w = Walk { cmd: "open-batch", args, i: 0 };
            let (mut repo, mut members, mut skip_pregate, mut dry_run) = (None, Text::None, false, false);
            while let Some(t) = w.next() {
                match t {
                    Tok::Opt(k, v) if members_opt(&mut w, k, v.clone(), &mut members) => {}
                    Tok::Opt("skip-pregate", None) => skip_pregate = true,
                    Tok::Opt("dry-run", None) => dry_run = true,
                    Tok::Opt(k, _) => return Err(w.unknown(k)),
                    Tok::Pos(p) => repo = Some(p.to_string()),
                }
            }
            Ok(Cmd::OpenBatch { repo, members, skip_pregate, dry_run })
        }
        "claim" => {
            let mut w = Walk { cmd: "claim", args, i: 0 };
            let (mut repo, mut reason, mut force) = (None, Text::None, false);
            while let Some(t) = w.next() {
                match t {
                    Tok::Opt(k, v) if reason_opt(&mut w, k, v.clone(), &mut reason) => {}
                    Tok::Opt("force", None) => force = true,
                    Tok::Opt(k, _) => return Err(w.unknown(k)),
                    Tok::Pos(p) => repo = Some(p.to_string()),
                }
            }
            Ok(Cmd::Claim { repo, reason, force })
        }
        "release" => Ok(Cmd::Release { repo: repo_only("release", args)? }),
        "land-local" => {
            let mut w = Walk { cmd: "land-local", args, i: 0 };
            let (mut repo, mut head, mut members, mut worktree) = (None, String::new(), Text::None, None);
            while let Some(t) = w.next() {
                match t {
                    Tok::Opt("head", v) => head = w.value(v),
                    Tok::Opt(k, v) if members_opt(&mut w, k, v.clone(), &mut members) => {}
                    Tok::Opt("worktree", v) => worktree = Some(PathBuf::from(w.value(v))),
                    Tok::Opt(k, _) => return Err(w.unknown(k)),
                    Tok::Pos(p) => repo = Some(p.to_string()),
                }
            }
            if head.is_empty() {
                return Err(Usage("queue.sh land-local: --head is required".into()));
            }
            if matches!(&members, Text::None) || matches!(&members, Text::Arg(s) if s.is_empty()) {
                return Err(Usage("queue.sh land-local: --members is required".into()));
            }
            Ok(Cmd::LandLocal { repo, head, members, worktree })
        }
        "publish" => Ok(Cmd::Publish { repo: repo_only("publish", args)? }),
        "publish-settle" => Ok(Cmd::PublishSettle { repo: repo_only("publish-settle", args)? }),
        "to-forge" => Ok(Cmd::ToForge { repo: repo_only("to-forge", args)? }),
        "to-local" => Ok(Cmd::ToLocal { repo: repo_only("to-local", args)? }),
        // rollback-local took `${1:-home}` and nothing else.
        "rollback-local" => Ok(Cmd::RollbackLocal { repo: pos(0) }),
        "round" => parse_round(args).map(Cmd::Round),
        _ => Err(Usage(USAGE.into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &[&str]) -> Result<Cmd, Usage> {
        parse(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn unknown_subcommand_is_usage() {
        assert_eq!(p(&["nope"]), Err(Usage(USAGE.into())));
        assert_eq!(p(&[]), Err(Usage(USAGE.into())));
    }

    #[test]
    fn submit_needs_a_branch() {
        assert_eq!(p(&["submit"]), Err(Usage("queue.sh submit: branch name required".into())));
        assert_eq!(p(&["submit", "spira/sp-a", "spira"]), Ok(Cmd::Submit { branch: "spira/sp-a".into(), repo: Some("spira".into()) }));
    }

    #[test]
    fn eject_reason_in_both_spellings_and_red() {
        let c = p(&["eject", "sp-a", "--reason=r1", "--red", "spira"]).unwrap();
        assert_eq!(c, Cmd::Eject { id: "sp-a".into(), repo: Some("spira".into()), reason: Text::Arg("r1".into()), suites: String::new(), red: true, harness_fault: false, dry_run: false });
        let c = p(&["eject", "sp-a", "--reason-file", "-", "--suites", "t.sh"]).unwrap();
        assert!(matches!(c, Cmd::Eject { reason: Text::Stdin, ref suites, .. } if suites == "t.sh"));
        assert_eq!(p(&["eject", "sp-a", "--bogus"]), Err(Usage("queue.sh eject: unknown option: --bogus".into())));
        assert_eq!(p(&["eject"]), Err(Usage("queue.sh eject: bead id required".into())));
    }

    #[test]
    fn land_local_requires_head_and_members() {
        assert_eq!(p(&["land-local", "spira", "--members", "a:1"]), Err(Usage("queue.sh land-local: --head is required".into())));
        assert_eq!(p(&["land-local", "--head", "x"]), Err(Usage("queue.sh land-local: --members is required".into())));
        let c = p(&["land-local", "spira", "--head", "abc", "--members-file", "/tmp/m", "--worktree", "/w"]).unwrap();
        assert_eq!(c, Cmd::LandLocal { repo: Some("spira".into()), head: "abc".into(), members: Text::File("/tmp/m".into()), worktree: Some("/w".into()) });
    }

    #[test]
    fn round_verbs_parse_their_positionals_and_options() {
        let c = p(&["round", "open", "spira", "--members", "a:1,b", "--name", "r1", "--worktree", "/w"]).unwrap();
        assert_eq!(c, Cmd::Round(Round::Open { repo: Some("spira".into()), members: Text::Arg("a:1,b".into()), name: Some("r1".into()), worktree: Some("/w".into()) }));
        let c = p(&["round", "eject", "b1", "sp-a", "--reason=r", "--no-rebuild", "--suites", "t.sh"]).unwrap();
        assert!(matches!(c, Cmd::Round(Round::Eject { ref batch, ref id, rebuild: false, ref suites, .. }) if batch == "b1" && id == "sp-a" && suites == "t.sh"));
        assert_eq!(p(&["round", "land", "b1"]), Ok(Cmd::Round(Round::Land { batch: "b1".into(), repo: None })));
        assert_eq!(p(&["round", "status"]), Ok(Cmd::Round(Round::Status { repo: None })));
        assert_eq!(p(&["round", "pass-start", "b1"]), Ok(Cmd::Round(Round::PassStart { batch: "b1".into(), repo: None })));
        assert_eq!(p(&["round", "suites-started", "b1", "spira"]), Ok(Cmd::Round(Round::SuitesStarted { batch: "b1".into(), repo: Some("spira".into()) })));
        let c = p(&["round", "pass-verdict", "b1", "--verdict", "red", "--red-suites", "t.sh", "--suites-s", "90", "--build-s=30"]).unwrap();
        assert!(matches!(c, Cmd::Round(Round::PassVerdict { ref verdict, ref red_suites, suites_s: 90, build_s: 30, .. }) if verdict == "red" && red_suites == "t.sh"));
        assert!(p(&["round", "pass-verdict", "b1"]).is_err(), "a verdict is required");
        assert!(p(&["round", "pass-verdict", "b1", "--verdict", "green", "--suites-s", "x"]).is_err());
    }

    #[test]
    fn preempt_names_its_ejects_and_its_evidence() {
        let c = p(&["round", "preempt", "b1", "spira", "--eject", "sp-a,sp-b", "--reason", "red", "--suites=test-x.sh"]).unwrap();
        assert!(matches!(c, Cmd::Round(Round::Preempt { ref batch, ref repo, ref eject, ref suites, .. })
            if batch == "b1" && repo.as_deref() == Some("spira") && eject == "sp-a,sp-b" && suites == "test-x.sh"));
        assert_eq!(p(&["round", "preempt", "b1"]), Err(Usage("queue.sh round preempt: --eject <id>[,<id>...] is required".into())));
    }

    #[test]
    fn staged_round_verbs_parse() {
        let c = p(&["round", "stage", "spira", "--members", "a:1", "--name", "r2"]).unwrap();
        assert_eq!(c, Cmd::Round(Round::Stage { repo: Some("spira".into()), members: Text::Arg("a:1".into()), name: Some("r2".into()), worktree: None }));
        assert_eq!(p(&["round", "stage"]), Err(Usage("queue.sh round stage: --members is required".into())));
        assert_eq!(p(&["round", "stage-test"]), Ok(Cmd::Round(Round::StageTest { repo: None })));
        assert_eq!(p(&["round", "promote", "spira"]), Ok(Cmd::Round(Round::Promote { repo: Some("spira".into()) })));
        assert_eq!(p(&["round", "discard", "--reason", "x"]), Ok(Cmd::Round(Round::Discard { repo: None, reason: Text::Arg("x".into()) })));
    }

    #[test]
    fn round_verbs_refuse_what_they_need() {
        assert_eq!(p(&["round", "open"]), Err(Usage("queue.sh round open: --members is required".into())));
        assert_eq!(p(&["round", "certify"]), Err(Usage("queue.sh round certify: batch id required".into())));
        assert_eq!(p(&["round", "eject", "b1"]), Err(Usage("queue.sh round eject: bead id required".into())));
        assert_eq!(p(&["round", "land", "b1", "--bogus"]), Err(Usage("queue.sh round land: unknown option: --bogus".into())));
        assert_eq!(p(&["round", "nope"]), Err(Usage(USAGE.into())));
        assert_eq!(p(&["round"]), Err(Usage(USAGE.into())));
    }

    #[test]
    fn repo_only_commands_refuse_options() {
        assert_eq!(p(&["publish", "-x"]), Err(Usage("queue.sh publish: unknown option: -x".into())));
        assert_eq!(p(&["to-forge", "spira"]), Ok(Cmd::ToForge { repo: Some("spira".into()) }));
    }
}
