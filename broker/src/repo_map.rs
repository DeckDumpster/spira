// repo_map.rs — parse a repo-map line into a local checkout path. execute.rs and read.rs
// each carried their own copy of this; one copy is also the one that gets a unit test.

use std::path::PathBuf;

pub fn parse(content: &str, repo: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.splitn(6, '|').map(str::trim).collect();
        if fields.len() >= 2 && fields[0] == repo {
            return Some(fields[1].to_string());
        }
    }
    None
}

pub fn lookup(repo: &str) -> Result<Option<String>, String> {
    let map_path: PathBuf = {
        // SPIRA_REPO_MAP is a registered key — resolved from spira.toml, no fallback.
        let v = spira_config::process::cfg("SPIRA_REPO_MAP")?;
        if !v.is_empty() {
            PathBuf::from(v)
        } else {
            // SPIRA_HOME is not a registered config key (spira/conf.d has no entry) —
            // left on the process environment.
            let home = std::env::var("SPIRA_HOME").unwrap_or_default();
            PathBuf::from(home).join("repo-map")
        }
    };
    match std::fs::read_to_string(&map_path) {
        Ok(content) => Ok(parse(&content, repo)),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_matching_repo() {
        let map = "test-repo | /srv/checkouts/test-repo | push | origin/main | | \n";
        assert_eq!(
            parse(map, "test-repo"),
            Some("/srv/checkouts/test-repo".to_string())
        );
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let map = "# a comment\n\ntest-repo | /srv/checkouts/test-repo | push | origin/main | | \n";
        assert_eq!(
            parse(map, "test-repo"),
            Some("/srv/checkouts/test-repo".to_string())
        );
    }

    #[test]
    fn missing_repo_is_none() {
        let map = "other | /srv/checkouts/other | push | origin/main | | \n";
        assert_eq!(parse(map, "test-repo"), None);
    }

    #[test]
    fn picks_first_matching_row_when_duplicated() {
        let map = "test-repo | /first | push | origin/main | | \ntest-repo | /second | push | origin/main | | \n";
        assert_eq!(parse(map, "test-repo"), Some("/first".to_string()));
    }

    #[test]
    fn a_short_row_with_no_path_column_is_skipped() {
        let map = "test-repo\n";
        assert_eq!(parse(map, "test-repo"), None);
    }
}
