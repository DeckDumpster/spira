//! Explicit suite lists (`--suites a,b` and `--suites -`). Diff-derived selection is
//! the suite-select crate's (the ONE selector); run.rs links it.

/// A named suite does not exist in the suite directory: exit 2, naming it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSuite(pub String);

fn collect<'a>(
    names: impl Iterator<Item = &'a str>,
    exists: &dyn Fn(&str) -> bool,
) -> Result<Vec<String>, UnknownSuite> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let n = n.trim();
        if n.is_empty() {
            continue;
        }
        if !exists(n) {
            return Err(UnknownSuite(n.to_string()));
        }
        if !out.iter().any(|s| s == n) {
            out.push(n.to_string());
        }
    }
    Ok(out)
}

/// Every `test-*.sh` regular file in `dir`, basenames, sorted. The glob is the definition.
pub fn population(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("test-") && n.ends_with(".sh"))
        .collect();
    v.sort();
    v
}

/// `--suites a.sh,b.sh`: comma list, whitespace trimmed, duplicates dropped, order kept.
pub fn from_list(list: &str, exists: &dyn Fn(&str) -> bool) -> Result<Vec<String>, UnknownSuite> {
    collect(list.split(','), exists)
}

/// `--suites -`: one name per line; blank lines ignored; empty input selects nothing.
pub fn from_lines(text: &str, exists: &dyn Fn(&str) -> bool) -> Result<Vec<String>, UnknownSuite> {
    collect(text.lines(), exists)
}

/// A name is only ever a file directly in the suite directory.
pub fn plausible_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && name != "." && name != ".."
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(n: &str) -> bool {
        matches!(n, "test-a.sh" | "test-b.sh")
    }

    #[test_id(thtg7t9)]
    #[test]
    fn list_trims_dedups_and_keeps_order() {
        assert_eq!(
            from_list(" test-b.sh , test-a.sh,,test-b.sh", &known).unwrap(),
            vec!["test-b.sh", "test-a.sh"]
        );
    }

    #[test_id(tg90p0r)]
    #[test]
    fn unknown_name_is_named() {
        assert_eq!(
            from_list("test-a.sh,test-z.sh", &known),
            Err(UnknownSuite("test-z.sh".into()))
        );
    }

    #[test_id(tz5hqts)]
    #[test]
    fn stdin_ignores_blank_lines_and_empty_means_nothing() {
        assert_eq!(
            from_lines("\ntest-a.sh\n  \ntest-b.sh\n", &known).unwrap(),
            vec!["test-a.sh", "test-b.sh"]
        );
        assert!(from_lines("", &known).unwrap().is_empty());
        assert!(from_lines("\n\n", &known).unwrap().is_empty());
    }

    #[test_id(t0qijlk)]
    #[test]
    fn path_like_names_are_not_suites() {
        assert!(plausible_name("test-a.sh"));
        assert!(!plausible_name("../etc/passwd"));
        assert!(!plausible_name(".."));
    }

    #[test_id(txz4gav)]
    #[test]
    fn population_is_the_glob() {
        let d = testkit::TempDir::new("suites-pop");
        std::fs::create_dir_all(d.join("test-dir.sh")).unwrap();
        for f in ["test-b.sh", "test-a.sh", "lib.sh", "test-a.sh.bak", "test-.sh"] {
            std::fs::write(d.join(f), "").unwrap();
        }
        assert_eq!(population(&d), vec!["test-.sh", "test-a.sh", "test-b.sh"]);
        assert!(population(&d.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
