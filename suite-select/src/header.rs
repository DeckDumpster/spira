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
}
