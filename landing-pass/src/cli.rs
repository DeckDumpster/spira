//! Argument parsing (DESIGN.md §2.1).

#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    /// `--pass` / `pr`: the pr-mode pass.
    Pr,
    /// `land`: the gated pass.
    Land,
    Halt { reason: Reason, dry_run: bool },
    SweepRed,
    Help,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reason {
    None,
    Text(String),
    /// `--reason-file F`, `-` for stdin.
    File(String),
}

pub const USAGE: &str = "usage: landing-pass --pass | land | halt [--reason T | --reason-file F|-] [--dry-run] | sweep-red";

/// Err((exit code, message for stderr)).
pub fn parse(args: &[String]) -> Result<Cmd, (i32, String)> {
    match args.first().map(String::as_str) {
        Some("--pass") | Some("pr") if args.len() == 1 => Ok(Cmd::Pr),
        Some("land") if args.len() == 1 => Ok(Cmd::Land),
        Some("sweep-red") if args.len() == 1 => Ok(Cmd::SweepRed),
        Some("-h") | Some("--help") => Ok(Cmd::Help),
        Some("halt") => parse_halt(&args[1..]),
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
        assert_eq!(parse(&v(&["sweep-red"])), Ok(Cmd::SweepRed));
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
    }
}
