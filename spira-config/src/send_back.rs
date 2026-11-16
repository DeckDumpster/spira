//! A send-back's reason as a lifecycle fact: `tip <sha>`, a blank line, the reason verbatim.
//! The `tip ` prefix is the one `ejection::parse` reads, so one tip check covers both kinds.

pub const KIND: &str = "send-back";
const TIP_PREFIX: &str = "tip ";

pub fn encode(tip: &str, reason: &str) -> String {
    format!("{TIP_PREFIX}{tip}\n\n{reason}")
}

/// The tip and the reason; a cause without the prefix has no tip and is all reason.
pub fn parse(cause: &str) -> (String, String) {
    let Some(rest) = cause.strip_prefix(TIP_PREFIX) else {
        return (String::new(), cause.to_string());
    };
    match rest.split_once("\n\n") {
        Some((tip, reason)) => (tip.trim().to_string(), reason.to_string()),
        None => (rest.trim().to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_with_newlines_and_quotes_round_trips() {
        let r = "not ok 2 - \"a spike\"\n\nwanted [1] got [0]\n'x'\n";
        assert_eq!(
            parse(&encode("abc123", r)),
            ("abc123".to_string(), r.to_string())
        );
        assert_eq!(
            super::super::ejection::parse(&encode("abc123", r)).tip,
            "abc123"
        );
    }
}
