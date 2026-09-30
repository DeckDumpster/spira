//! The snapshot: `$SPIRA_RUN/cockpit.env`, written by `spira/cockpit.sh`'s `write_snapshot`.
//!
//! FORMAT. Every line is `KEY='value'`, single-quoted with bash's own escaping (`'` inside a
//! value becomes the four characters `'\''`) — `write_snapshot`'s python post-pass wraps every
//! raw `KEY=value` line this way before it ever reaches disk, specifically so that sourcing it
//! in bash (`. "$SPIRA_SNAP"`) reproduces the value exactly, spaces and all. This module parses
//! that same format directly rather than shelling out to `bash -c 'source ...; env'`: reading a
//! snapshot is on the hot path (a repaint every 2s) and must not fork.
//!
//! A key is accepted only when it starts with a letter or `_` (mirrors the python writer's own
//! filter, which is itself what makes a stray non-KV line in the probe's output harmless rather
//! than corrupting the next key). The first occurrence of a key wins — `write_snapshot` already
//! dedupes on its way to disk, but a second, independent reader must make the same promise
//! rather than trust the file was always written by that pass.

use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct Snapshot {
    kv: HashMap<String, String>,
}

impl Snapshot {
    pub fn parse(content: &str) -> Snapshot {
        let mut kv = HashMap::new();
        for line in content.lines() {
            let Some(idx) = line.find('=') else { continue };
            let key = &line[..idx];
            if key.is_empty() {
                continue;
            }
            let first = key.as_bytes()[0];
            if !(first.is_ascii_alphabetic() || first == b'_') {
                continue;
            }
            if kv.contains_key(key) {
                continue; // first occurrence wins
            }
            let raw = &line[idx + 1..];
            kv.insert(key.to_string(), unquote_single(raw));
        }
        Snapshot { kv }
    }

    /// The raw value, or `None` if the key was never written. Callers apply whatever default
    /// bash's `${VAR:-x}` used at that call site — they differ line to line, so this stays
    /// undefaulted rather than guessing one.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.kv.get(key).map(|s| s.as_str())
    }

    /// `get(key).unwrap_or("?")` — far and away the commonest default in the original.
    pub fn q(&self, key: &str) -> &str {
        self.get(key).unwrap_or("?")
    }

    /// Every key whose value is exactly `"timeout"` or `"error"` and whose name starts with
    /// `_PROBE_STATUS_`, with that prefix stripped — `header_line`'s stale-probe roll call.
    /// Order matches the snapshot file's own line order (insertion order isn't preserved by
    /// `HashMap`, so this re-derives it from the source text rather than the parsed map).
    pub fn stalled_probes(content: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in content.lines() {
            let Some(rest) = line.strip_prefix("_PROBE_STATUS_") else { continue };
            let Some(idx) = rest.find('=') else { continue };
            let name = &rest[..idx];
            let val = unquote_single(&rest[idx + 1..]);
            if val == "timeout" || val == "error" {
                out.push(name.to_string());
            }
        }
        out
    }

    pub fn is_empty_map(&self) -> bool {
        self.kv.is_empty()
    }
}

/// Reverse `write_snapshot`'s quoting: strip the surrounding `'...'` and turn every `'\''`
/// back into a literal `'`. A value that (unexpectedly) arrives unquoted is passed through
/// as-is rather than mangled — a defensive fallback, not a format the writer ever produces.
fn unquote_single(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        let inner = &raw[1..raw.len() - 1];
        inner.replace("'\\''", "'")
    } else {
        raw.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_values_round_trip() {
        let s = Snapshot::parse("SP_OPEN='42'\nSP_READY='?'\n");
        assert_eq!(s.get("SP_OPEN"), Some("42"));
        assert_eq!(s.get("SP_READY"), Some("?"));
        assert_eq!(s.get("SP_MISSING"), None);
        assert_eq!(s.q("SP_MISSING"), "?");
    }

    #[test]
    fn embedded_single_quotes_and_spaces_survive() {
        // The exact escaping write_snapshot's python pass produces for a value containing a
        // literal quote: close, backslash-escaped quote, reopen.
        let s = Snapshot::parse("SP_QUEUE_BATCH0='P1 sp-abc Don'\\''t stop title'\n");
        assert_eq!(s.get("SP_QUEUE_BATCH0"), Some("P1 sp-abc Don't stop title"));
    }

    #[test]
    fn first_occurrence_wins_on_duplicate_key() {
        let s = Snapshot::parse("SP_OPEN='1'\nSP_OPEN='2'\n");
        assert_eq!(s.get("SP_OPEN"), Some("1"));
    }

    #[test]
    fn lines_with_no_equals_or_bad_leading_char_are_skipped() {
        let s = Snapshot::parse("not a kv line\n123BAD='x'\n_OK='y'\n=noval\n");
        assert_eq!(s.get("123BAD"), None);
        assert_eq!(s.get("_OK"), Some("y"));
    }

    #[test]
    fn stalled_probes_lists_timeout_and_error_only() {
        let content = "_PROBE_STATUS_foo='timeout'\n_PROBE_STATUS_bar='ok'\n_PROBE_STATUS_baz='error'\nOTHER='timeout'\n";
        assert_eq!(Snapshot::stalled_probes(content), vec!["foo".to_string(), "baz".to_string()]);
    }

    #[test]
    fn empty_value_is_empty_string_not_missing() {
        let s = Snapshot::parse("SP_X=''\n");
        assert_eq!(s.get("SP_X"), Some(""));
    }
}
