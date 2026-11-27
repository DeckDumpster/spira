use std::path::PathBuf;

pub const USAGE: &str = "usage: spira-lint [--root <dir>] [--only <rule>] [--base <rev> | --diff <rev>] [--emit-allow] [--findings <file>] [--deny-list <file>] [--gate-bead <id>]\n       spira-lint --only inventory --scan <file>";

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub root: Option<PathBuf>,
    pub only: Option<String>,
    pub base: Option<String>,
    pub scan: Option<PathBuf>,
    pub diff: bool,
    pub emit_allow: bool,
    pub findings_file: Option<PathBuf>,
    pub deny_list: Option<String>,
    pub gate_bead: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    Run(Args),
    Help,
}

pub fn parse_args(
    argv: impl Iterator<Item = String>,
    default_base: Option<String>,
) -> Result<Parsed, String> {
    let mut out = Args {
        base: default_base,
        ..Args::default()
    };
    let mut args = argv;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--root" => out.root = args.next().map(PathBuf::from),
            "--only" => out.only = args.next(),
            "--base" => out.base = args.next(),
            "--diff" => {
                out.base = args.next();
                out.diff = true;
            }
            "--emit-allow" => out.emit_allow = true,
            "--findings" => out.findings_file = args.next().map(PathBuf::from),
            "--scan" => out.scan = args.next().map(PathBuf::from),
            "--deny-list" => match args.next().filter(|v| !v.is_empty()) {
                Some(v) => out.deny_list = Some(v),
                None => {
                    return Err("spira-lint: --deny-list names no file — the caller handed no deny list (SPIRA_TEST_DENY_LIST); refusing to check test-id-row-orphan against none".into())
                }
            },
            "--gate-bead" => match args.next() {
                Some(v) => out.gate_bead = Some(v).filter(|v| !v.is_empty()),
                None => return Err(format!("spira-lint: --gate-bead takes an item id, or an empty value for none\n{USAGE}")),
            },
            "-h" | "--help" => return Ok(Parsed::Help),
            _ => return Err(format!("spira-lint: unknown argument {a}\n{USAGE}")),
        }
    }
    Ok(Parsed::Run(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_test_id::test_id;

    fn argv(a: &[&str]) -> impl Iterator<Item = String> {
        a.iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test_id(tt5ti2m)]
    #[test]
    fn an_empty_deny_list_value_is_refused_naming_the_missing_input() {
        let e = parse_args(argv(&["--deny-list", ""]), None).unwrap_err();
        assert!(
            e.contains("--deny-list names no file") && e.contains("SPIRA_TEST_DENY_LIST"),
            "{e}"
        );
        assert!(parse_args(argv(&["--deny-list"]), None).is_err());
    }

    #[test_id(taz1kyq)]
    #[test]
    fn a_deny_list_path_is_carried_onto_the_arguments() {
        let Ok(Parsed::Run(a)) = parse_args(argv(&["--deny-list", "x/deny"]), None) else {
            panic!()
        };
        assert_eq!(a.deny_list.as_deref(), Some("x/deny"));
    }

    #[test_id(tsd5s6w)]
    #[test]
    fn the_gated_item_is_carried_and_an_empty_value_names_none() {
        for (value, want) in [("sp-x", Some("sp-x")), ("", None)] {
            let Ok(Parsed::Run(a)) = parse_args(argv(&["--gate-bead", value]), None) else {
                panic!("{value:?}")
            };
            assert_eq!(a.gate_bead.as_deref(), want);
        }
        let e = parse_args(argv(&["--gate-bead"]), None).unwrap_err();
        assert!(e.contains("--gate-bead"), "{e}");
        assert!(USAGE.contains("--gate-bead <id>"), "{USAGE}");
    }
}
