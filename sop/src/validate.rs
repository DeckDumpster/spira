//! The one shape-validator every SOP is held to, whether it arrives through `write` or the
//! shelf's back door (`bd remember sop-<slug>` directly — `lint` catches what slipped past).
//!
//! **Unified, unlike the bash.** `sop.sh write` and `sop.sh validate`/`lint` enforced
//! slightly different rule sets: `write` checked SYMPTOM/CHECK/FIX, the MATCH regex and the
//! word cap inline, but never checked METRIC's shape — only `_sop_validate` (reached from
//! `lint`/`validate`) did. A malformed `METRIC:` line could be written clean and would only
//! be caught later, by `lint`, or never if `lint` is not run. This is the "one validator, no
//! shelf, no bd" comment already promised in the bash header and never quite delivered; this
//! rewrite delivers it (DESIGN.md "Decisions" — a dropped accreted accident, not a behaviour
//! anyone depended on keeping).

use regex::Regex;

pub struct Violation(pub String);

/// `validate(text, word_cap)` — every rule `write`, `lint` and `validate` enforce, in one
/// place, over a string. Empty violations means the SOP is well-formed. The key each
/// violation belongs to is the caller's to prepend (`write` and `validate` have exactly
/// one; `lint` loops over the shelf and prepends each one's own).
pub fn validate(text: &str, word_cap: usize) -> Vec<Violation> {
    let mut out = Vec::new();
    if text.trim().is_empty() {
        out.push(Violation("empty SOP".to_string()));
        return out;
    }
    for field in ["SYMPTOM", "CHECK", "FIX"] {
        if !field_re(field).is_match(text) {
            out.push(Violation(format!("missing required field: {field}")));
        }
    }
    if let Some(pat) = match_line(text) {
        if Regex::new(&pat).is_err() {
            out.push(Violation(format!("MATCH is not a valid extended regex: {pat}")));
        }
    }
    if let Some(spec) = field_value(text, "METRIC") {
        let parts: Vec<&str> = spec.split_whitespace().collect();
        let shape_ok = parts.len() == 2
            && key_re().is_match(parts[0])
            && subcmd_re().is_match(parts[1]);
        if !shape_ok {
            out.push(Violation(format!(
                "METRIC must be KEY SUBCMD — uppercase cockpit key then lowercase cockpit \
subcommand (e.g. SP_UNADOPTED unsent); got: {spec}"
            )));
        }
    }
    let words = text.split_whitespace().count();
    if words > word_cap {
        out.push(Violation(format!("{words} words, cap is {word_cap}")));
    }
    out
}

fn field_re(name: &str) -> Regex {
    Regex::new(&format!(r"(?m)^\s*{name}:")).expect("static")
}

/// The first `MATCH:` line's value, trimmed. `None` when there is no such line.
pub fn match_line(text: &str) -> Option<String> {
    field_value(text, "MATCH")
}

/// The first `METRIC:` line's value, trimmed. `None` when there is no such line.
pub fn metric_line(text: &str) -> Option<String> {
    field_value(text, "METRIC")
}

/// The first `<NAME>:` line's value (to end of line), trimmed. Mirrors the python's
/// single-line field extraction used for `MATCH`/`METRIC` at validation time — `synth`'s
/// multi-line field extraction (`field()` in `synth.rs`) is a different, richer read used
/// only for rendering the wiki page.
fn field_value(text: &str, name: &str) -> Option<String> {
    let re = Regex::new(&format!(r"(?m)^\s*{name}:\s*(.+)$")).expect("static");
    re.captures(text).map(|c| c[1].trim().to_string())
}

fn key_re() -> Regex {
    Regex::new(r"^[A-Z][A-Z0-9_]*$").expect("static")
}
fn subcmd_re() -> Regex {
    Regex::new(r"^[a-z][a-z0-9_-]*$").expect("static")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(v: Vec<Violation>) -> Vec<String> {
        v.into_iter().map(|x| x.0).collect()
    }

    #[test]
    fn empty_text_is_one_violation() {
        assert_eq!(msgs(validate("", 250)), vec!["empty SOP"]);
        assert_eq!(msgs(validate("   \n  ", 250)), vec!["empty SOP"]);
    }

    #[test]
    fn missing_fields_are_named_individually() {
        let v = msgs(validate("SYMPTOM: it broke", 250));
        assert!(v.contains(&"missing required field: CHECK".to_string()));
        assert!(v.contains(&"missing required field: FIX".to_string()));
        assert!(!v.iter().any(|m| m.contains("SYMPTOM")));
    }

    #[test]
    fn a_complete_sop_is_clean() {
        let text = "SYMPTOM: x\nCHECK: y\nFIX: z\n";
        assert!(validate(text, 250).is_empty());
    }

    #[test]
    fn a_bad_match_regex_is_caught() {
        let text = "MATCH: [unterminated\nSYMPTOM: x\nCHECK: y\nFIX: z\n";
        let v = msgs(validate(text, 250));
        assert!(v.iter().any(|m| m.contains("not a valid extended regex")), "{v:?}");
    }

    #[test]
    fn a_good_match_regex_is_not_flagged() {
        let text = "MATCH: disk.*full\nSYMPTOM: x\nCHECK: y\nFIX: z\n";
        assert!(validate(text, 250).is_empty());
    }

    #[test]
    fn metric_shape_rules() {
        let ok = "METRIC: SP_UNADOPTED unsent\nSYMPTOM: x\nCHECK: y\nFIX: z\n";
        assert!(validate(ok, 250).is_empty());

        for bad in [
            "METRIC: sp_unadopted unsent\nSYMPTOM: x\nCHECK: y\nFIX: z\n", // lowercase key
            "METRIC: SP_UNADOPTED UNSENT\nSYMPTOM: x\nCHECK: y\nFIX: z\n", // uppercase subcmd
            "METRIC: SP_UNADOPTED\nSYMPTOM: x\nCHECK: y\nFIX: z\n",        // one token
            "METRIC: SP_UNADOPTED unsent extra\nSYMPTOM: x\nCHECK: y\nFIX: z\n", // three tokens
        ] {
            let v = msgs(validate(bad, 250));
            assert!(v.iter().any(|m| m.contains("METRIC must be KEY SUBCMD")), "{bad:?} -> {v:?}");
        }
    }

    #[test]
    fn word_cap_is_enforced() {
        let long = format!("SYMPTOM: x\nCHECK: y\nFIX: {}\n", "word ".repeat(300));
        let v = msgs(validate(&long, 250));
        assert!(v.iter().any(|m| m.contains("cap is 250")), "{v:?}");
    }

    #[test]
    fn match_line_extracts_trimmed_value() {
        assert_eq!(match_line("MATCH:   disk full  \nSYMPTOM: x"), Some("disk full".to_string()));
        assert_eq!(match_line("SYMPTOM: x"), None);
    }
}
