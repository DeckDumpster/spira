//! The suite header parser: `# covers:`, `# tier:`, `# selects-on:`. The one Rust copy of
//! `spira/suite-covers.sh`'s accessors (spira-lint and batcher-cut read it from here).
//!
//! THE RULE `covers_of` ENCODES. A suite with no `# covers:` line — or one that is empty after
//! the prefix — covers everything and must never be skipped for want of a declaration: the
//! caller sees `None` and runs it always.

/// `suite_covers_of`: the first `# covers:` line's globs, folded with its continuation lines —
/// a comment line indented by two or more blanks that is not itself a `# word:` directive.
/// `None` when the suite declares nothing. (Like the bash, this reads the first `# covers:`
/// anywhere in the file.)
pub fn covers_of(text: &str) -> Option<Vec<String>> {
    let mut it = text.lines();
    let first = it.by_ref().find_map(|l| {
        let rest = l.strip_prefix('#')?.trim_start_matches(' ');
        rest.strip_prefix("covers:").map(|r| r.trim_start_matches(' ').to_string())
    })?;
    let mut out = first;
    for l in it {
        let Some(body) = l.strip_prefix('#') else { break };
        let blanks = body.len() - body.trim_start_matches([' ', '\t']).len();
        let rest = &body[blanks..];
        let is_directive = body.starts_with(' ')
            && body[1..].split_once(':').is_some_and(|(w, _)| {
                !w.is_empty()
                    && w.as_bytes()[0].is_ascii_alphabetic()
                    && w.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_' || b == b'-')
            });
        if blanks >= 2 && !rest.is_empty() && !rest.starts_with('#') && !is_directive {
            out.push(' ');
            out.push_str(rest);
        } else {
            break;
        }
    }
    Some(out.split_whitespace().map(str::to_string).collect())
}

/// The value of the first `# <key>:` line before the first `set -` line, as the bash's
/// `sed -n '/^set -/q;s/^# *<key>: *//p' | head -1` (the `set -` stop keeps a heredoc in the
/// suite body from spoofing a declaration).
fn directive(text: &str, key: &str) -> Option<String> {
    for l in text.lines() {
        if l.starts_with("set -") {
            return None;
        }
        if let Some(rest) = l.strip_prefix('#') {
            if let Some(v) = rest.trim_start_matches(' ').strip_prefix(key) {
                if let Some(v) = v.strip_prefix(':') {
                    return Some(v.trim_start_matches(' ').to_string());
                }
            }
        }
    }
    None
}

/// `suite_tier_of`: the raw `# tier:` value, or `None` when undeclared.
pub fn tier_of(text: &str) -> Option<String> {
    directive(text, "tier")
}

/// `suite_selects_on_of`: the `# selects-on:` event tokens (commas or blanks separate them).
pub fn selects_on_of(text: &str) -> Vec<String> {
    directive(text, "selects-on")
        .map(|v| {
            v.split(|c: char| c == ',' || c.is_whitespace())
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `suite_requires_of`: the `# requires:` tokens (commas or blanks separate them, so both
/// "claude, bd" and "claude bd" work). Empty when undeclared — the suite runs unconditionally.
pub fn requires_of(text: &str) -> Vec<String> {
    directive(text, "requires")
        .map(|v| {
            v.split(|c: char| c == ',' || c.is_whitespace())
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `suite_exclusive_of`: the `# exclusive:` reason string, or `None` when not exclusive.
pub fn exclusive_of(text: &str) -> Option<String> {
    directive(text, "exclusive")
}

/// `suite_uc_of`: the `UC-<area>-NN` tokens on the `# covers:` line — path globs never take
/// this shape, so filtering `covers_of` picks out exactly the use-case ids.
pub fn uc_of(text: &str) -> Vec<String> {
    covers_of(text).into_iter().flatten().filter(|t| is_uc_token(t)).collect()
}

/// `case "$_tok" in UC-*-[0-9][0-9]) ;; esac`: starts with `UC-`, ends with two digits
/// preceded by a `-` that is NOT the same character as the prefix's own `-` (so the
/// minimum match is 6 bytes, e.g. `UC--00`; `UC-01` at 5 bytes does not match).
fn is_uc_token(tok: &str) -> bool {
    let b = tok.as_bytes();
    b.len() >= 6
        && tok.starts_with("UC-")
        && b[b.len() - 3] == b'-'
        && b[b.len() - 2].is_ascii_digit()
        && b[b.len() - 1].is_ascii_digit()
}

/// `suite_testenv_unmet`: true iff the suite declares `# requires: testenv` and the caller
/// is NOT both inside the test container (structural evidence) AND has set
/// `SPIRA_IN_TESTENV=1`. `requires` is this suite's own `requires_of(text)`; `in_testenv` and
/// `in_container` are the caller's environment evidence (kept out of this pure module —
/// see `suite_in_container`'s own doc for why a variable alone is never enough).
pub fn testenv_unmet(requires: &[String], in_testenv: bool, in_container: bool) -> bool {
    requires.iter().any(|r| r == "testenv") && !(in_testenv && in_container)
}

/// A declared tier. An undeclared tier is `None` at the call site, never a `Tier`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    T0,
    T1,
    T2,
    T3,
    T4,
}

impl Tier {
    pub fn parse(s: &str) -> Option<Tier> {
        Some(match s {
            "T0" => Tier::T0,
            "T1" => Tier::T1,
            "T2" => Tier::T2,
            "T3" => Tier::T3,
            "T4" => Tier::T4,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::T0 => "T0",
            Tier::T1 => "T1",
            Tier::T2 => "T2",
            Tier::T3 => "T3",
            Tier::T4 => "T4",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_folds_continuations_and_stops_at_a_directive() {
        let t = "#!/bin/bash\n# covers: a.sh b.sh\n#   c/* UC-x-01\n# tier: T1\n#   d.sh\nset -u\n";
        assert_eq!(covers_of(t).unwrap(), ["a.sh", "b.sh", "c/*", "UC-x-01"]);
        assert_eq!(covers_of("# covers:\n").unwrap(), Vec::<String>::new());
        assert_eq!(covers_of("#!/bin/bash\nset -u\n"), None);
        // A lone `#` ends the block; one blank of indent is not a continuation.
        assert_eq!(covers_of("# covers: a\n#\n#   b\n").unwrap(), ["a"]);
        assert_eq!(covers_of("# covers: a\n# b\n").unwrap(), ["a"]);
        // `#covers:` with no blank is still the declaration (`^# *covers:`).
        assert_eq!(covers_of("#covers: x\n").unwrap(), ["x"]);
    }

    #[test]
    fn tier_and_selects_on_stop_at_set_dash() {
        let t = "# tier: T2\n# selects-on: added,mode\nset -uo pipefail\n# tier: T0\n";
        assert_eq!(tier_of(t).as_deref(), Some("T2"));
        assert_eq!(selects_on_of(t), ["added", "mode"]);
        let spoof = "set -u\ncat <<X\n# tier: T3\n# selects-on: added\nX\n";
        assert_eq!(tier_of(spoof), None);
        assert!(selects_on_of(spoof).is_empty());
        assert_eq!(selects_on_of("# selects-on: added mode\n"), ["added", "mode"]);
        assert_eq!(tier_of("# tier:\n").as_deref(), Some(""));
    }

    #[test]
    fn tiers_parse_exactly() {
        assert_eq!(Tier::parse("T3"), Some(Tier::T3));
        assert_eq!(Tier::parse("t1"), None);
        assert_eq!(Tier::parse("T5"), None);
        assert_eq!(Tier::T4.as_str(), "T4");
    }

    // Mirrors spira/test-requires.sh Part A: requires_of parses, comma or blank delimited,
    // empty when undeclared, and stops at `set -` like every other directive.
    #[test]
    fn requires_parses_commas_and_blanks_and_stops_at_set_dash() {
        assert_eq!(requires_of("# requires: claude, bd\n"), ["claude", "bd"]);
        assert_eq!(requires_of("# requires: claude bd\n"), ["claude", "bd"]);
        assert!(requires_of("#!/bin/bash\nset -u\n").is_empty());
        let spoof = "set -u\ncat <<X\n# requires: testenv\nX\n";
        assert!(requires_of(spoof).is_empty());
    }

    // Mirrors spira/test-dummy.sh's exclusive_of coverage and the "stops at set -" guard
    // suite_exclusive_of shares with suite_requires_of / suite_selects_on_of.
    #[test]
    fn exclusive_is_the_reason_string_or_none() {
        assert_eq!(exclusive_of("# exclusive: touches the live store\n").as_deref(), Some("touches the live store"));
        assert_eq!(exclusive_of("# tier: T1\n"), None);
        let spoof = "set -u\ncat <<X\n# exclusive: spoofed\nX\n";
        assert_eq!(exclusive_of(spoof), None);
    }

    // Mirrors plan-lint.sh's suite_uc_of use: UC ids on # covers:, path globs never match.
    #[test]
    fn uc_of_picks_only_uc_shaped_tokens_off_covers() {
        let t = "# covers: a.sh b/*.sh UC-covers-01 UC-plan-02\n";
        assert_eq!(uc_of(t), ["UC-covers-01", "UC-plan-02"]);
        assert!(uc_of("# covers: a.sh *.sh\n").is_empty());
        assert!(uc_of("# tier: T1\n").is_empty());
        // UC-01 is 5 bytes: "UC-" + "*" + "-DD" needs >= 6, so this does NOT match, exactly
        // as the bash glob `UC-*-[0-9][0-9]` would not match it either.
        assert!(!is_uc_token("UC-01"));
        assert!(is_uc_token("UC--00"));
        assert!(is_uc_token("UC-covers-01"));
        assert!(!is_uc_token("UCX-01-02"));
    }

    // Mirrors spira/test-testenv-guard.sh Part A: testenv_unmet is the AND/NOT composition
    // of "declares requires: testenv" with the caller's own (in_testenv, in_container) pair.
    #[test]
    fn testenv_unmet_is_the_and_not_of_requires_and_evidence() {
        let reqs = requires_of("# requires: testenv\n");
        assert!(testenv_unmet(&reqs, false, false));
        assert!(testenv_unmet(&reqs, true, false));
        assert!(testenv_unmet(&reqs, false, true));
        assert!(!testenv_unmet(&reqs, true, true));
        let other = requires_of("# requires: claude\n");
        assert!(!testenv_unmet(&other, false, false));
        let none = requires_of("# tier: T1\n");
        assert!(!testenv_unmet(&none, false, false));
    }
}
