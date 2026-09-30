//! The snapshot's `SP_*`/`YIELD_*` key-value map, read from `cockpit.env` and from
//! `yield.sh report`'s stdout — both plain `KEY=value` lines, never full shell syntax, so
//! parsing is a per-line prefix match, not `eval`. `g()` is the bash helper of the same
//! name: an absent or empty key renders `?`, never `0` (law-absence-needs-a-positive-control).

use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct Env(BTreeMap<String, String>);

impl Env {
    pub fn new() -> Self {
        Env(BTreeMap::new())
    }

    /// Parses every line shaped `<prefix><REST>=<value>` (the bash's
    /// `sed -n 's/^\(PREFIX[A-Z_0-9]*\)=\(.*\)$/.../p'`), merging into this map. Lines that
    /// don't match are ignored, same as the sed filter.
    pub fn merge_lines(&mut self, text: &str, prefix: &str) {
        for line in text.lines() {
            let Some(rest) = line.strip_prefix(prefix) else {
                continue;
            };
            let Some(eq) = rest.find('=') else { continue };
            let key_rest = &rest[..eq];
            if !key_rest.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
                continue;
            }
            let key = format!("{prefix}{key_rest}");
            let value = rest[eq + 1..].to_string();
            self.0.insert(key, value);
        }
    }

    #[cfg(test)]
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.insert(key.into(), value.into());
    }

    /// `g()`: the value if present and non-empty, else `?`.
    pub fn g(&self, key: &str) -> &str {
        match self.0.get(key) {
            Some(v) if !v.is_empty() => v,
            _ => "?",
        }
    }

    /// The raw value (possibly empty), for callers that need to distinguish "empty" from
    /// "absent" themselves (the sweep's numeric-threshold checks do their own `!= "?"` test
    /// after calling `g`, so this is rarely needed, but kept for completeness).
    pub fn raw(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|s| s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g_renders_unknown_for_absent_and_empty_never_zero() {
        let mut e = Env::new();
        e.set("SP_FOO", "");
        assert_eq!(e.g("SP_FOO"), "?");
        assert_eq!(e.g("SP_MISSING"), "?");
        e.set("SP_FOO", "3");
        assert_eq!(e.g("SP_FOO"), "3");
    }

    #[test]
    fn merge_lines_only_takes_matching_prefixed_assignments() {
        let mut e = Env::new();
        e.merge_lines(
            "SP_UNLANDED_N=4\nnot a match\nSP_AT=1700000000\n# comment\nSP_lowercase=skip\n",
            "SP_",
        );
        assert_eq!(e.g("SP_UNLANDED_N"), "4");
        assert_eq!(e.g("SP_AT"), "1700000000");
        assert_eq!(e.g("SP_lowercase"), "?");
    }

    #[test]
    fn merge_lines_handles_the_yield_prefix_with_quoted_values() {
        // yield.sh's own output is re-quoted by the bash (`\1="\2"`) before eval; here the
        // value is whatever text follows `=` verbatim, quotes and all, matching what a
        // reader of the RAW yield.sh output (before the bash's own re-quoting) would see.
        let mut e = Env::new();
        e.merge_lines("YIELD_REDS=3\nYIELD_RECORDER=silent\n", "YIELD_");
        assert_eq!(e.g("YIELD_REDS"), "3");
        assert_eq!(e.g("YIELD_RECORDER"), "silent");
    }
}
