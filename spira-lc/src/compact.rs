//! `spira-lc admin-store-compact`: the operator's one-time conversion of the stopped control
//! store's main file to incremental auto-vacuum. Never reached from `serve` or from any open.

use std::path::Path;

use state_store_sqlite::compact::compact;

use crate::{CANNOT_TELL, REFUSED};

pub const USAGE: &str = "usage: spira-lc admin-store-compact --store <main file>

Rewrites the stopped store's main file with incremental auto-vacuum, giving back its free
pages, and prints a JSON report on stdout. Day files are not touched.

  --store <file>   the main file; its <file>.lock must be free, so stop lc-serve first
  -h               this text

exit 0 done, 3 refused (the store is open, or its -wal is not empty), 2 usage";

#[derive(Debug, PartialEq, Eq)]
pub enum Asked {
    Help,
    Run { store: String },
}

pub fn parse(args: &[String]) -> Result<Asked, String> {
    let mut store = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(Asked::Help),
            "--store" => match it.next() {
                Some(v) if !v.starts_with('-') => store = Some(v.clone()),
                _ => return Err("--store needs a path".into()),
            },
            bare if bare.starts_with('-') => return Err(format!("unknown flag {bare}; the flag is --store")),
            bare => return Err(format!("{bare:?} is a bare word; name it: --store {bare}")),
        }
    }
    store.map(|store| Asked::Run { store }).ok_or_else(|| "--store <main file> is required".into())
}

/// Exit code and the stdout text: JSON on every path but `-h`.
pub fn admin_store_compact(args: &[String]) -> (i32, String) {
    match parse(args) {
        Ok(Asked::Help) => (0, USAGE.to_string()),
        Err(e) => (CANNOT_TELL, serde_json::json!({ "compact": "usage", "error": e, "usage": USAGE }).to_string()),
        Ok(Asked::Run { store }) => match compact(Path::new(&store)) {
            Ok(report) => (0, report.to_json().to_string()),
            Err(e) => (REFUSED, serde_json::json!({ "compact": "refused", "error": e.to_string() }).to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn the_store_flag_is_taken_by_name() {
        assert_eq!(parse(&s(&["--store", "a.db"])), Ok(Asked::Run { store: "a.db".into() }));
    }

    #[test]
    fn a_missing_flag_a_bare_word_and_a_flag_with_no_path_are_refused() {
        assert!(parse(&s(&[])).unwrap_err().contains("--store"));
        assert!(parse(&s(&["a.db"])).unwrap_err().contains("--store a.db"));
        assert!(parse(&s(&["--store", "--x"])).unwrap_err().contains("needs a path"));
    }

    #[test]
    fn a_usage_error_exits_2_with_json_and_help_lists_the_flag_and_the_exits() {
        let (code, out) = admin_store_compact(&s(&["a.db"]));
        assert_eq!(code, 2);
        assert!(serde_json::from_str::<serde_json::Value>(&out).unwrap()["error"].as_str().unwrap().contains("--store a.db"));
        let (code, help) = admin_store_compact(&s(&["-h"]));
        assert_eq!(code, 0);
        for want in ["--store", "0 done", "3 refused", "2 usage"] {
            assert!(help.contains(want), "{want}");
        }
    }

    #[test]
    fn a_refusal_exits_3_with_json_on_stdout() {
        let dir = testkit::TempDir::new("lc-compact-refused");
        let missing = dir.path().join("missing.db");
        let (code, out) = admin_store_compact(&s(&["--store", &missing.display().to_string()]));
        assert_eq!(code, 3);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["compact"], "refused");
        assert!(v["error"].as_str().unwrap().contains("missing.db"));
    }
}
