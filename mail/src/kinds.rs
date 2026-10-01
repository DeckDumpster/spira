//! Mail "kind" files (`spira/mail/kinds/<kind>.md`): a frontmatter block between two `---`
//! lines carrying `requires: <Header> <Header>`, then a body template whose `## Section`
//! headings are the sections `send` requires non-empty. Editing a kind file changes
//! acceptance with no code change (test-mail.sh "editing a kind file...").

use std::path::{Path, PathBuf};

pub fn kind_file(dir: &Path, kind: &str) -> PathBuf {
    dir.join(format!("{kind}.md"))
}

pub fn kind_exists(dir: &Path, kind: &str) -> bool {
    kind_file(dir, kind).is_file()
}

#[derive(Debug, Default, Clone)]
pub struct Kind {
    /// Header names named on the frontmatter's `requires:` line (whitespace-split, as the
    /// bash `for h in $req` word-splits its unquoted variable).
    pub requires: Vec<String>,
    /// `## Section` names appearing after the frontmatter, in order.
    pub sections: Vec<String>,
    /// Everything after the closing `---` line, verbatim — the body `mail template` prints.
    pub template: String,
}

/// Parses a kind file's text. Two lines that are exactly `---` delimit the frontmatter;
/// `requires:` is read only from within it (delim == 1); sections and the template come
/// from everything after the second `---` line.
pub fn parse(text: &str) -> Kind {
    let lines: Vec<&str> = text.lines().collect();
    let mut delim = 0usize;
    let mut requires_line: Option<&str> = None;
    let mut template_start: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        if *line == "---" {
            delim += 1;
            if delim == 2 && template_start.is_none() {
                template_start = Some(i + 1);
            }
            continue;
        }
        if delim == 1 && requires_line.is_none() {
            if let Some(rest) = line.strip_prefix("requires:") {
                requires_line = Some(rest.trim_start());
            }
        }
    }

    let start = template_start.unwrap_or(lines.len());
    let body_lines = &lines[start.min(lines.len())..];
    let sections = body_lines
        .iter()
        .filter_map(|l| l.strip_prefix("## ").map(|s| s.to_string()))
        .collect();
    let template = body_lines.join("\n");
    let requires = requires_line
        .unwrap_or("")
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    Kind { requires, sections, template }
}

pub fn load(dir: &Path, kind: &str) -> Option<Kind> {
    let text = std::fs::read_to_string(kind_file(dir, kind)).ok()?;
    Some(parse(&text))
}

/// `_section_empty`: true if `## <section>` is missing from `body`, or present with no
/// non-whitespace content before the next `##` heading or end of body.
pub fn section_empty(section: &str, body: &str) -> bool {
    let heading = format!("## {section}");
    let mut found = false;
    for line in body.lines() {
        if !found {
            if line == heading {
                found = true;
            }
            continue;
        }
        if line.starts_with("##") {
            return true;
        }
        if line.trim().is_empty() {
            continue;
        }
        return false;
    }
    // Either never found, or found and ran out of body with nothing but blanks.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUESTION: &str = "---\nrequires: X-Spira-Default\n---\n\n## Question\n\n## Default\n";
    const NOTE: &str = "---\n---\n\n## Note\n";

    #[test]
    fn requires_is_read_only_from_the_frontmatter() {
        let k = parse(QUESTION);
        assert_eq!(k.requires, vec!["X-Spira-Default".to_string()]);
        assert_eq!(k.sections, vec!["Question".to_string(), "Default".to_string()]);
    }

    #[test]
    fn a_kind_with_no_requires_line_requires_nothing() {
        let k = parse(NOTE);
        assert!(k.requires.is_empty());
        assert_eq!(k.sections, vec!["Note".to_string()]);
    }

    #[test]
    fn section_empty_true_when_heading_absent() {
        assert!(section_empty("Note", "## Other\n\ncontent\n"));
    }

    #[test]
    fn section_empty_true_when_heading_has_only_blank_lines() {
        assert!(section_empty("Note", "## Note\n\n\n## Next\ncontent\n"));
    }

    #[test]
    fn section_empty_false_when_content_present() {
        assert!(!section_empty("Note", "## Note\n\nSome content here.\n"));
    }

    #[test]
    fn section_empty_stops_at_the_next_heading() {
        assert!(!section_empty("Note", "## Note\n\nok\n\n## Next\n"));
    }
}
