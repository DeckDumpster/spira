//! Reading a delivered message's headers. Two distinct scans, because mail.sh used two:
//! `list`/`tidy` grep the whole file case-sensitively (mail.sh's own `sed -n
//! 's/^Subject:[[:space:]]*//p' "$f"`), while `sendmail` stops at the first blank line and
//! matches case-insensitively (its `awk` reply parser) — a message whose body happens to
//! contain a line starting with a header name must not confuse the reply router.

/// Splits at the first blank line. `(headers, body)`; `body` is `""` if there is none.
pub fn split_headers_body(text: &str) -> (&str, &str) {
    // RFC 5322 mail is CRLF-terminated (aerc sends it that way); the blank line ending the
    // headers is then "\r\n\r\n". Whichever separator comes first wins.
    let lf = text.find("\n\n").map(|i| (i, 2));
    let crlf = text.find("\r\n\r\n").map(|i| (i, 4));
    match [lf, crlf].into_iter().flatten().min_by_key(|(i, _)| *i) {
        Some((idx, n)) => (&text[..idx], &text[idx + n..]),
        None => (text, ""),
    }
}

/// The one place a header is written: line breaks fold per RFC 5322 2.2.3 (break plus one
/// leading space, text kept); a bare CR or a NUL cannot be made legal and is refused.
pub fn header(name: &str, value: &str) -> Result<String, String> {
    if value.contains('\0') {
        return Err(format!("mail: header {name} contains a NUL — refused"));
    }
    let crlf_folded = value.replace("\r\n", "\n");
    if crlf_folded.contains('\r') {
        return Err(format!("mail: header {name} contains a bare CR — refused"));
    }
    Ok(format!("{name}: {}\n", crlf_folded.replace('\n', "\n ")))
}

/// `line` plus its folded continuations from `rest`, unfolded per RFC 5322: only the line
/// break is removed, so each folded newline reads back as the space that replaced it.
fn unfolded<'a>(line: &'a str, rest: impl Iterator<Item = &'a str>) -> String {
    let mut out = line.trim_end_matches('\r').to_string();
    for next in rest {
        if !next.starts_with(' ') && !next.starts_with('\t') {
            break;
        }
        out.push_str(next.trim_end_matches('\r'));
    }
    out
}

fn value_after_colon(unfolded: &str) -> String {
    unfolded.split_once(':').map(|(_, v)| v.trim_start().to_string()).unwrap_or_default()
}

/// Whole-file, case-sensitive `^Name:` scan, first match, value trimmed of leading
/// whitespace only — `list`/`tidy`'s header reads.
pub fn header_line_sed(text: &str, name: &str) -> String {
    let prefix = format!("{name}:");
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.starts_with(&prefix) {
            return value_after_colon(&unfolded(line, lines.clone()));
        }
    }
    String::new()
}

/// Headers-only (stops at the first empty line; a whitespace-only line is a fold, not the
/// end), case-insensitive — `sendmail`'s reply-header reads.
pub fn header_ci_before_blank(text: &str, name_lower: &str) -> String {
    let want = format!("{name_lower}:");
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim_end_matches('\r').is_empty() {
            break;
        }
        if line.to_lowercase().starts_with(&want) {
            return value_after_colon(&unfolded(line, lines.clone()));
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    #[test]
    fn split_headers_body_handles_crlf_mail() {
        let raw = "From: A\r\nSubject: S\r\n\r\nAnswer line.\r\n";
        let (h, b) = super::split_headers_body(raw);
        assert_eq!(h, "From: A\r\nSubject: S");
        assert_eq!(b.trim(), "Answer line.");
    }

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
    fn a_folded_header_is_written_legal_and_reads_back_with_newlines_as_spaces() {
        let written = header("X-Spira-Default", "one\n\ntwo\r\nthree").unwrap();
        assert_eq!(written, "X-Spira-Default: one\n \n two\n three\n");
        let msg = format!("From: a\n{written}\nbody\n");
        assert_eq!(header_ci_before_blank(&msg, "x-spira-default"), "one  two three");
        assert_eq!(header_line_sed(&msg, "X-Spira-Default"), "one  two three");
    }

    #[test]
    fn a_bare_cr_or_nul_is_refused_naming_the_header() {
        assert!(header("Subject", "a\rb").unwrap_err().contains("Subject"));
        assert!(header("X-Spira-Bead", "a\0b").unwrap_err().contains("X-Spira-Bead"));
    }

    #[test]
    fn missing_header_is_empty() {
        assert_eq!(header_line_sed(MSG, "X-Nope"), "");
        assert_eq!(header_ci_before_blank(MSG, "x-nope"), "");
    }
}
