//! Argument parsing (DESIGN.md §2.1). Usage errors carry testenv-batch.sh's own messages,
//! because callers (batcher-cut) read some of them off stderr.

use crate::record::Mode;

pub const USAGE: &str = "usage: testenv [--mode parallel|serial] [--suites <list|->] [--profile <p>] [--with-bins] [--report [N]] <branch> [<repo-name>]";

/// Where the explicit suite list comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuitesArg {
    List(String),
    Stdin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunArgs {
    pub mode: Mode,
    pub suites: Option<SuitesArg>,
    pub profile: String,
    /// `--with-bins` was given (logged; only picks the profile, DESIGN.md D1).
    pub with_bins: bool,
    pub branch: String,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Report(usize),
    Run(RunArgs),
}

/// A usage error: message lines for stderr; exit status 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError(pub Vec<String>);

fn err(lines: &[&str]) -> UsageError {
    UsageError(lines.iter().map(|s| s.to_string()).collect())
}

pub fn parse(args: &[String]) -> Result<Invocation, UsageError> {
    let mut mode = "parallel".to_string();
    let mut suites: Option<String> = None;
    let mut profile: Option<String> = None;
    let mut with_bins = false;
    let mut report: Option<usize> = None;
    let mut i = 0;
    let set_suites = |suites: &mut Option<String>, v: String| -> Result<(), UsageError> {
        if suites.is_some() {
            return Err(err(&["batch: --suites may only be given once"]));
        }
        *suites = Some(v);
        Ok(())
    };
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--mode" | "--suites" | "--profile" => {
                let v = args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| UsageError(vec![format!("batch: {a} requires an argument")]))?;
                match a {
                    "--mode" => mode = v,
                    "--suites" => set_suites(&mut suites, v)?,
                    _ => profile = Some(v),
                }
                i += 2;
            }
            "--with-bins" => {
                with_bins = true;
                i += 1;
            }
            "--report" => {
                match args.get(i + 1).and_then(|v| {
                    v.parse::<usize>()
                        .ok()
                        .filter(|_| v.starts_with(|c: char| c.is_ascii_digit()))
                }) {
                    Some(n) => {
                        report = Some(n);
                        i += 2;
                    }
                    None => {
                        report = Some(20);
                        i += 1;
                    }
                }
            }
            "--" => {
                i += 1;
                break;
            }
            _ if a.starts_with("--mode=") => {
                mode = a["--mode=".len()..].to_string();
                i += 1;
            }
            _ if a.starts_with("--suites=") => {
                set_suites(&mut suites, a["--suites=".len()..].to_string())?;
                i += 1;
            }
            _ if a.starts_with("--profile=") => {
                profile = Some(a["--profile=".len()..].to_string());
                i += 1;
            }
            _ if a.starts_with("--report=") => {
                report = Some(a["--report=".len()..].parse().map_err(|_| {
                    UsageError(vec![format!("batch: --report needs a number, got: {a}")])
                })?);
                i += 1;
            }
            _ if a.starts_with('-') && a.len() > 1 => {
                return Err(UsageError(vec![
                    format!("batch: unknown option: {a}"),
                    USAGE.to_string(),
                ]));
            }
            _ => break,
        }
    }
    if let Some(n) = report {
        return Ok(Invocation::Report(n));
    }
    let mode = Mode::parse(&mode).ok_or_else(|| {
        UsageError(vec![format!(
            "batch: --mode must be parallel or serial, got: {mode}"
        )])
    })?;
    let rest = &args[i..];
    let branch = rest
        .first()
        .filter(|b| !b.is_empty())
        .cloned()
        .ok_or_else(|| err(&[USAGE]))?;
    let repo = rest.get(1).filter(|r| !r.is_empty()).cloned();
    let profile = profile.unwrap_or_else(|| {
        if with_bins {
            "release".into()
        } else {
            "aeon".into()
        }
    });
    if profile.is_empty()
        || !profile
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(UsageError(vec![format!(
            "batch: --profile must be a cargo profile name, got: {profile}"
        )]));
    }
    let suites = suites.filter(|s| !s.is_empty()).map(|s| {
        if s == "-" {
            SuitesArg::Stdin
        } else {
            SuitesArg::List(s)
        }
    });
    Ok(Invocation::Run(RunArgs {
        mode,
        suites,
        profile,
        with_bins,
        branch,
        repo,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Result<Invocation, UsageError> {
        parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }
    fn run(a: &[&str]) -> RunArgs {
        match p(a).unwrap() {
            Invocation::Run(r) => r,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn defaults_are_parallel_aeon_diff() {
        let r = run(&["spira/sp-x"]);
        assert_eq!(r.mode, Mode::Parallel);
        assert_eq!(r.profile, "aeon");
        assert_eq!(r.suites, None);
        assert_eq!(r.branch, "spira/sp-x");
        assert_eq!(r.repo, None);
    }

    #[test]
    fn round_invocation_parses_and_with_bins_means_release() {
        let r = run(&[
            "--mode",
            "parallel",
            "--with-bins",
            "--suites",
            "test-a.sh,test-b.sh",
            "concierge/round-5",
        ]);
        assert_eq!(r.profile, "release");
        assert!(r.with_bins);
        assert_eq!(
            r.suites,
            Some(SuitesArg::List("test-a.sh,test-b.sh".into()))
        );
    }

    #[test]
    fn explicit_profile_wins_over_with_bins() {
        assert_eq!(
            run(&["--with-bins", "--profile", "aeon", "b"]).profile,
            "aeon"
        );
        assert_eq!(
            run(&["--profile=release", "b", "/srv/repo"])
                .repo
                .as_deref(),
            Some("/srv/repo")
        );
    }

    #[test]
    fn stdin_suites_and_equals_forms() {
        let r = run(&["--mode=serial", "--suites=-", "HEAD"]);
        assert_eq!(r.mode, Mode::Serial);
        assert_eq!(r.suites, Some(SuitesArg::Stdin));
    }

    #[test]
    fn suites_twice_is_refused() {
        let e = p(&["--suites", "a", "--suites=b", "x"]).unwrap_err();
        assert_eq!(e.0, vec!["batch: --suites may only be given once"]);
    }

    #[test]
    fn unknown_option_names_it_and_prints_usage() {
        let e = p(&["--frobnicate", "x"]).unwrap_err();
        assert_eq!(e.0[0], "batch: unknown option: --frobnicate");
        assert_eq!(e.0[1], USAGE);
    }

    #[test]
    fn bad_mode_and_missing_branch_are_usage_errors() {
        assert!(p(&["--mode", "fast", "x"]).unwrap_err().0[0]
            .contains("--mode must be parallel or serial"));
        assert_eq!(p(&[]).unwrap_err().0, vec![USAGE.to_string()]);
        assert!(p(&["--mode"]).is_err());
    }

    #[test]
    fn report_takes_an_optional_number() {
        assert_eq!(p(&["--report"]).unwrap(), Invocation::Report(20));
        assert_eq!(p(&["--report", "5"]).unwrap(), Invocation::Report(5));
        assert_eq!(p(&["--report=7"]).unwrap(), Invocation::Report(7));
        assert_eq!(p(&["--report", "HEAD"]).unwrap(), Invocation::Report(20));
    }

    #[test]
    fn profile_names_are_checked() {
        assert!(p(&["--profile", "../x", "b"]).is_err());
    }
}
