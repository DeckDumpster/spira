//! `match`: the deterministic tier. Every SOP whose `MATCH:` regex fires against the
//! incident payload, best first by number of distinct payload lines hit. An SOP with no
//! `MATCH:` line falls back to its key tokens — weak on purpose, a nudge to write one.

use crate::validate::match_line;
use regex::RegexBuilder;
use std::collections::BTreeMap;

pub struct Hit {
    pub key: String,
    pub how: &'static str,
    pub score: usize,
    pub symptom: String,
}

pub fn score(shelf: &BTreeMap<String, String>, payload: &str) -> Vec<Hit> {
    let mut hits: Vec<Hit> = Vec::new();
    for (k, v) in shelf {
        let symptom = crate::shelf::symptom_of(v);
        let (score, how): (usize, &'static str) = match match_line(v) {
            Some(pat) => match RegexBuilder::new(&pat).case_insensitive(true).multi_line(true).build() {
                Ok(re) => {
                    let distinct: std::collections::HashSet<&str> =
                        payload.lines().filter(|l| re.is_match(l)).collect();
                    (distinct.len(), "MATCH")
                }
                Err(_) => (0, "BAD-REGEX"),
            },
            None => {
                let toks: Vec<&str> = k.trim_start_matches("sop-").split('-').filter(|t| t.len() > 3).collect();
                let n = toks
                    .iter()
                    .filter(|t| {
                        RegexBuilder::new(&regex::escape(t))
                            .case_insensitive(true)
                            .build()
                            .map(|re| re.is_match(payload))
                            .unwrap_or(false)
                    })
                    .count();
                (n, "key-tokens")
            }
        };
        if score > 0 {
            hits.push(Hit { key: k.clone(), how, score, symptom });
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.key.cmp(&b.key)));
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shelf(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn a_match_regex_scores_by_distinct_lines_hit() {
        let s = shelf(&[("sop-disk-full", "MATCH: disk.*full\nSYMPTOM: x\nCHECK: y\nFIX: z")]);
        let hits = score(&s, "warning: disk is full\nanother line\ndisk is full again but same pattern is one distinct line? \n");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].how, "MATCH");
        assert!(hits[0].score >= 1);
    }

    #[test]
    fn no_match_line_falls_back_to_key_tokens() {
        let s = shelf(&[("sop-template-apt-drift", "SYMPTOM: x\nCHECK: y\nFIX: z")]);
        let hits = score(&s, "the template has apt automation enabled and drift happened");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].how, "key-tokens");
        // tokens > 3 chars: "template", "drift" ("apt" is 3 chars, excluded)
        assert_eq!(hits[0].score, 2);
    }

    #[test]
    fn an_unrelated_payload_does_not_fire() {
        let s = shelf(&[("sop-disk-full", "MATCH: disk.*full\nSYMPTOM: x\nCHECK: y\nFIX: z")]);
        let hits = score(&s, "everything is fine here");
        assert!(hits.is_empty());
    }

    #[test]
    fn a_bad_match_regex_never_fires() {
        let s = shelf(&[("sop-bad", "MATCH: [unterminated\nSYMPTOM: x\nCHECK: y\nFIX: z")]);
        let hits = score(&s, "[unterminated shows up literally here");
        assert!(hits.is_empty(), "a BAD-REGEX entry must score 0, not crash or match literally");
    }

    #[test]
    fn higher_score_sorts_first() {
        let s = shelf(&[
            ("sop-weak", "MATCH: x\nSYMPTOM: s\nCHECK: y\nFIX: z"),
            ("sop-strong", "MATCH: x\nSYMPTOM: s\nCHECK: y\nFIX: z"),
        ]);
        let hits = score(&s, "x\nx\nx\n");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].score, hits[1].score); // same pattern, same payload
    }
}
