//! Which diffs are text: the declared allow-list (`SPIRA_QUEUE_TEXT_ONLY`) read as paths.

/// `dir/` matches a prefix, `*.ext` a suffix, anything else one exact path.
fn matches(entry: &str, path: &str) -> bool {
    if let Some(suffix) = entry.strip_prefix('*') {
        path.ends_with(suffix)
    } else if entry.ends_with('/') {
        path.starts_with(entry)
    } else {
        path == entry
    }
}

/// True when `paths` is non-empty and every path matches an `allow` entry. A diff that names
/// no path, or an empty list, is never text: it fails closed into the round.
pub fn is_text_only(paths: &[String], allow: &[String]) -> bool {
    !paths.is_empty()
        && paths
            .iter()
            .all(|p| allow.iter().any(|e| matches(e, p)))
}

pub fn parse_allow(declared: &str) -> Vec<String> {
    declared.split_whitespace().map(str::to_string).collect()
}

/// The line `round open` gives a member that lands by lint instead.
pub fn skip_line(id: &str) -> String {
    format!("{id}: text-only: lands by lint")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_diff_of_declared_directories_and_extensions_is_text() {
        let allow = v(&["law/", "*.md", "NOTES"]);
        assert!(is_text_only(&v(&["law/1-general/x.toml", "a/b.md", "NOTES"]), &allow));
    }

    #[test]
    fn one_source_file_makes_the_diff_not_text() {
        let allow = v(&["law/", "*.md"]);
        assert!(!is_text_only(&v(&["law/x.md", "queue/src/lib.rs"]), &allow));
    }

    #[test]
    fn an_empty_diff_or_an_empty_allow_list_is_never_text() {
        assert!(!is_text_only(&[], &v(&["law/"])));
        assert!(!is_text_only(&v(&["law/x.md"]), &[]));
    }

    #[test]
    fn a_directory_entry_does_not_match_a_sibling_sharing_its_prefix() {
        assert!(!is_text_only(&v(&["lawless/x"]), &v(&["law/"])));
    }

    #[test]
    fn the_skip_line_says_it_lands_by_lint() {
        assert_eq!(skip_line("sp-a"), "sp-a: text-only: lands by lint");
    }
}
