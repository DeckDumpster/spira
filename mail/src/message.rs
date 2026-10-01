//! Reading a delivered message's headers. Two distinct scans, because mail.sh used two:
//! `list`/`tidy` grep the whole file case-sensitively (mail.sh's own `sed -n
//! 's/^Subject:[[:space:]]*//p' "$f"`), while `sendmail` stops at the first blank line and
//! matches case-insensitively (its `awk` reply parser) — a message whose body happens to
//! contain a line starting with a header name must not confuse the reply router.

/// Splits at the first blank line. `(headers, body)`; `body` is `""` if there is none.
pub fn split_headers_body(text: &str) -> (&str, &str) {
    match text.find("\n\n") {
        Some(idx) => (&text[..idx], &text[idx + 2..]),
        None => (text, ""),
    }
}

/// Whole-file, case-sensitive `^Name:` scan, first match, value trimmed of leading
/// whitespace only — `list`/`tidy`'s header reads.
pub fn header_line_sed(text: &str, name: &str) -> String {
    let prefix = format!("{name}:");
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(&prefix) {
            return rest.trim_start().to_string();
        }
    }
    String::new()
}

/// Headers-only (stops at the first blank/whitespace-only line), case-insensitive —
/// `sendmail`'s reply-header reads.
pub fn header_ci_before_blank(text: &str, name_lower: &str) -> String {
    let want = format!("{name_lower}:");
    for line in text.lines() {
        if line.trim().is_empty() {
            break;
        }
        let lower = line.to_lowercase();
        if lower.starts_with(&want) {
            if let Some(idx) = line.find(':') {
                return line[idx + 1..].trim_start().to_string();
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MSG: &str = "From: A <a@a>\nSubject: Hello\nX-Spira-Bead: sp-a\n\nBody text here.\nSubject: not a header, just text\n";

    #[test]
    fn split_finds_the_first_blank_line() {
        let (h, b) = split_headers_body(MSG);
        assert!(h.contains("Subject: Hello"));
        assert!(b.starts_with("Body text here."));
    }

    #[test]
    fn header_line_sed_scans_the_whole_file() {
        // Matches mail.sh's own sed behaviour: it is not scoped to the header block, so a
        // second "Subject:"-prefixed line in the body would also match — head -1 in the
        // original keeps the first one, which is what this returns too.
        assert_eq!(header_line_sed(MSG, "Subject"), "Hello");
    }

    #[test]
    fn header_ci_before_blank_stops_at_the_blank_line() {
        assert_eq!(header_ci_before_blank(MSG, "x-spira-bead"), "sp-a");
        assert_eq!(header_ci_before_blank(MSG, "subject"), "Hello");
    }

    #[test]
    fn header_ci_before_blank_is_case_insensitive_on_the_name() {
        assert_eq!(header_ci_before_blank("In-Reply-To: <abc@spira>\n\nbody\n", "in-reply-to"), "<abc@spira>");
    }

    #[test]
    fn missing_header_is_empty() {
        assert_eq!(header_line_sed(MSG, "X-Nope"), "");
        assert_eq!(header_ci_before_blank(MSG, "x-nope"), "");
    }
}
