//! The `SPIRA_ACTIONABLE` filter shared by `drain`, `tail` and `notify` (`_wd_filter`).
//! ONE expression for all three, because two implementations of "which lines need a
//! reader" is how `drain` and `tail` came to disagree in the bash. An empty expression is
//! refused rather than treated as "match everything": a mistyped or blanked config value
//! would otherwise silently turn the filter off.

#[cfg_attr(not(test), allow(dead_code))]
pub const DEFAULT: &str = "ANSWERED|COMMENTED|ESCALAT|STRANDED|POISON|DEGRADED|BLOCKED|UNREACHABLE|FAIL|ERROR|LANDED|⚠ BRANCH";

pub struct Filter(regex::Regex);

impl Filter {
    /// `pattern` empty is refused: as an ERE it would match every line, which is `--all`
    /// asked for silently rather than deliberately.
    pub fn compile(pattern: &str) -> Result<Filter, String> {
        if pattern.is_empty() {
            return Err("SPIRA_ACTIONABLE is empty — that would match every line; pass --all to ask for that deliberately".to_string());
        }
        regex::Regex::new(pattern)
            .map(Filter)
            .map_err(|e| format!("SPIRA_ACTIONABLE is not a usable pattern: {e}"))
    }

    pub fn matches(&self, line: &str) -> bool {
        self.0.is_match(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_pattern_is_refused() {
        assert!(Filter::compile("").is_err());
    }

    #[test]
    fn the_default_pattern_matches_the_words_it_names() {
        let f = Filter::compile(DEFAULT).unwrap();
        for word in ["ANSWERED", "COMMENTED", "ESCALATED", "STRANDED", "POISON", "DEGRADED", "BLOCKED", "UNREACHABLE", "FAIL", "ERROR", "LANDED"] {
            assert!(f.matches(&format!("2026-09-30T00:00:00Z something {word} happened")), "{word} should match");
        }
        assert!(f.matches("⚠ BRANCH stale"));
    }

    #[test]
    fn the_default_pattern_does_not_match_ordinary_progress_lines() {
        let f = Filter::compile(DEFAULT).unwrap();
        assert!(!f.matches("2026-09-30T00:00:00Z pool: NEW CERTIFIED sp-abc12"));
    }
}
