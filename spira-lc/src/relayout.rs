//! `spira-lc admin-store-relayout`: the operator's build of the new-layout control store from a
//! stopped old one. Never reached from `serve` or from any open.

use std::path::Path;

use runtime::ports::Clock;
use state_store_sqlite::relayout::relayout;

use crate::{CANNOT_TELL, REFUSED};

pub const USAGE: &str = "usage: spira-lc admin-store-relayout --from <old main file> --to <new main file>

Builds a new-layout store at --to from the stopped store at --from: its live keys exactly as
stored, the audit events of the last 14 days and the receipts of the last 24 hours in their
day files beside --to. It never writes --from, and prints a JSON report on stdout.

  --from <file>   the old main file; its <file>.lock must be free, so stop lc-serve first
  --to <file>     the new main file; it and its day files must not exist
  -h              this text

exit 0 done, 3 refused (the store is open, --to exists, the old file's -wal is not empty,
the old file is newer than this binary, or certification failed), 2 usage";

/// What a command line asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Asked {
    Help,
    Run { from: String, to: String },
}

pub fn parse(args: &[String]) -> Result<Asked, String> {
    let (mut from, mut to) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let slot = match a.as_str() {
            "-h" | "--help" => return Ok(Asked::Help),
            "--from" => &mut from,
            "--to" => &mut to,
            bare if bare.starts_with('-') => return Err(format!("unknown flag {bare}; the flags are --from and --to")),
            bare => {
                let flag = if from.is_none() { "--from" } else { "--to" };
                return Err(format!("{bare:?} is a bare word; name it: {flag} {bare}"));
            }
        };
        match it.next() {
            Some(v) if !v.starts_with('-') => *slot = Some(v.clone()),
            _ => return Err(format!("{a} needs a path")),
        }
    }
    match (from, to) {
        (Some(from), Some(to)) => Ok(Asked::Run { from, to }),
        (None, _) => Err("--from <old main file> is required".into()),
        (_, None) => Err("--to <new main file> is required".into()),
    }
}

/// Exit code and the stdout text: JSON on every path but `-h`.
pub fn admin_store_relayout(args: &[String], wall: &dyn Clock) -> (i32, String) {
    match parse(args) {
        Ok(Asked::Help) => (0, USAGE.to_string()),
        Err(e) => (CANNOT_TELL, serde_json::json!({ "relayout": "usage", "error": e, "usage": USAGE }).to_string()),
        Ok(Asked::Run { from, to }) => match relayout(Path::new(&from), Path::new(&to), wall) {
            Ok(report) => (0, report.to_json().to_string()),
            Err(e) => (REFUSED, serde_json::json!({ "relayout": "refused", "error": e.to_string() }).to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::Clock;

    struct Fixed(u64);

    impl Clock for Fixed {
        fn now_ms(&self) -> u64 {
            self.0
        }
    }

    fn s(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    #[test_id(t89kj33)]
    #[test]
    fn both_flags_by_name_are_taken_in_either_order() {
        let want = Asked::Run { from: "a.db".into(), to: "b.db".into() };
        assert_eq!(parse(&s(&["--from", "a.db", "--to", "b.db"])), Ok(want));
        assert_eq!(parse(&s(&["--to", "b.db", "--from", "a.db"])), Ok(Asked::Run { from: "a.db".into(), to: "b.db".into() }));
    }

    #[test_id(tzg9l4u)]
    #[test]
    fn a_missing_flag_is_named() {
        assert!(parse(&s(&["--to", "b.db"])).unwrap_err().contains("--from"));
        assert!(parse(&s(&["--from", "a.db"])).unwrap_err().contains("--to"));
        assert!(parse(&s(&[])).unwrap_err().contains("--from"));
    }

    #[test_id(toa778o)]
    #[test]
    fn a_bare_word_is_refused_naming_the_flag_it_should_have_been() {
        assert!(parse(&s(&["a.db", "b.db"])).unwrap_err().contains("--from a.db"));
        assert!(parse(&s(&["--from", "a.db", "b.db"])).unwrap_err().contains("--to b.db"));
    }

    #[test_id(t76hreb)]
    #[test]
    fn a_flag_with_no_path_is_refused() {
        assert!(parse(&s(&["--from", "--to", "b.db"])).unwrap_err().contains("--from needs a path"));
    }

    #[test_id(t5od5gw)]
    #[test]
    fn a_usage_error_exits_2_with_json_and_help_lists_both_flags_and_the_exits() {
        let (code, out) = admin_store_relayout(&s(&["a.db"]), &Fixed(0));
        assert_eq!(code, 2);
        assert!(serde_json::from_str::<serde_json::Value>(&out).unwrap()["error"].as_str().unwrap().contains("--from a.db"));
        let (code, help) = admin_store_relayout(&s(&["-h"]), &Fixed(0));
        assert_eq!(code, 0);
        for want in ["--from", "--to", "0 done", "3 refused", "2 usage"] {
            assert!(help.contains(want), "{want}");
        }
    }

    #[test_id(tvz3wp0)]
    #[test]
    fn a_refusal_exits_3_with_json_on_stdout_and_leaves_no_new_file() {
        let dir = testkit::TempDir::new("lc-relayout-refused");
        let to = dir.path().join("new.db");
        let from = dir.path().join("missing.db");
        let (code, out) = admin_store_relayout(&s(&["--from", &from.display().to_string(), "--to", &to.display().to_string()]), &Fixed(0));
        assert_eq!(code, 3);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["relayout"], "refused");
        assert!(v["error"].as_str().unwrap().contains("missing.db"));
        assert!(!to.exists());
    }
}
