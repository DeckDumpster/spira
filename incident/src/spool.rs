//! The write-ahead spool format (incident.sh's `spool_write`/`spool_field`/`spool_body`).
//! A spool entry is written to disk BEFORE the database is touched, and removed only once
//! the bead exists — so a production event that arrives while Dolt is down, or mid-reboot,
//! or while `bd` is over its timeout, is never lost: `drain` retries it later.

use std::path::{Path, PathBuf};

/// Everything captured about one event at spool time, so a `drain` retry sees exactly
/// what the original filing would have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpoolEntry {
    pub reference: String,
    pub title: String,
    pub unit: String,
    pub incident_path: String,
    pub cause: String,
    pub body: Vec<u8>,
}

/// Builds the on-disk text for one spool entry — a fixed header block, `--`, then the raw
/// payload. Byte-identical to incident.sh's `spool_write` so a spool directory populated by
/// the bash mid-cutover still drains correctly under the new binary.
pub fn format_entry(e: &SpoolEntry) -> Vec<u8> {
    let mut out = format!(
        "REF: {}\nTITLE: {}\nUNIT: {}\nINCIDENT_PATH: {}\nCAUSE: {}\n--\n",
        e.reference, e.title, e.unit, e.incident_path, e.cause
    )
    .into_bytes();
    out.extend_from_slice(&e.body);
    out
}

/// `spool_field <name>`: the first line of the form "<name>: <value>".
fn field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{name}: ");
    text.lines().find_map(|l| l.strip_prefix(prefix.as_str()))
}

/// `spool_body`: everything after the first line that is exactly "--".
fn body_bytes(raw: &[u8]) -> Vec<u8> {
    let marker = b"\n--\n";
    if let Some(pos) = raw.windows(marker.len()).position(|w| w == marker) {
        raw[pos + marker.len()..].to_vec()
    } else if raw.starts_with(b"--\n") {
        raw[3..].to_vec()
    } else {
        Vec::new()
    }
}

/// Parses a spool file's bytes back into an entry. `None` when it carries no REF line — a
/// malformed entry the caller moves aside to `.bad` rather than retrying forever.
pub fn parse_entry(raw: &[u8]) -> Option<SpoolEntry> {
    // The header is ASCII by construction (spool_write only ever writes REF/TITLE/UNIT/
    // INCIDENT_PATH/CAUSE as plain text); only the body may be arbitrary bytes, which is
    // why the header is decoded lossily but the body is sliced from the original bytes.
    let text = String::from_utf8_lossy(raw);
    let reference = field(&text, "REF")?.to_string();
    let title = field(&text, "TITLE").unwrap_or("").to_string();
    let unit = field(&text, "UNIT").unwrap_or("?").to_string();
    let incident_path = field(&text, "INCIDENT_PATH").unwrap_or("?").to_string();
    let cause = field(&text, "CAUSE").unwrap_or("").to_string();
    Some(SpoolEntry { reference, title, unit, incident_path, cause, body: body_bytes(raw) })
}

/// A filesystem-safe filename component: incident.sh's `tr -c 'A-Za-z0-9._-' '_' | cut -c1-80`.
pub fn safe_ref_component(reference: &str) -> String {
    let mut out: String = reference
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '_' })
        .collect();
    out.truncate(80);
    out
}

pub fn entry_path(spool_dir: &Path, stamp: &str, reference: &str, pid: u32) -> PathBuf {
    spool_dir.join(format!("{stamp}-{}-{pid}", safe_ref_component(reference)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_then_parse_round_trips() {
        let e = SpoolEntry {
            reference: "incident:foo".into(),
            title: "foo failed".into(),
            unit: "foo.service".into(),
            incident_path: "?".into(),
            cause: "systemd-fail".into(),
            body: b"line one\nline two\n".to_vec(),
        };
        let raw = format_entry(&e);
        let parsed = parse_entry(&raw).unwrap();
        assert_eq!(parsed, e);
    }

    #[test]
    fn parse_entry_with_no_ref_line_is_none() {
        assert_eq!(parse_entry(b"TITLE: x\n--\nbody\n"), None);
    }

    #[test]
    fn parse_entry_backward_compatible_with_missing_fields() {
        let raw = b"REF: incident:old\nTITLE: old event\n--\npayload\n";
        let parsed = parse_entry(raw).unwrap();
        assert_eq!(parsed.reference, "incident:old");
        assert_eq!(parsed.unit, "?");
        assert_eq!(parsed.incident_path, "?");
        assert_eq!(parsed.cause, "");
        assert_eq!(parsed.body, b"payload\n");
    }

    #[test]
    fn safe_ref_component_replaces_and_truncates() {
        assert_eq!(safe_ref_component("incident:a/b c"), "incident_a_b_c");
        let long = "x".repeat(200);
        assert_eq!(safe_ref_component(&long).len(), 80);
    }

    #[test]
    fn body_preserves_arbitrary_bytes_after_marker() {
        let mut raw = b"REF: r\nTITLE: t\nUNIT: u\nINCIDENT_PATH: p\nCAUSE: c\n--\n".to_vec();
        raw.extend_from_slice(&[0xff, 0x00, 0xfe, b'\n']);
        let parsed = parse_entry(&raw).unwrap();
        assert_eq!(parsed.body, vec![0xff, 0x00, 0xfe, b'\n']);
    }
}
