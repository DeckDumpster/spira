//! Suite names and suite lists — the contract the gate crate's re-entry check and base
//! re-run share with the selector's ejected suites.

/// A suite name the runner accepts: `test-<name>.sh`, one path component, no shell
/// metacharacters (it is interpolated into commands).
pub fn is_suite_name(s: &str) -> bool {
    s.len() > "test-.sh".len()
        && s.starts_with("test-")
        && s.ends_with(".sh")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// A suite list as the `$SPIRA_RUN/ejected/<id>` sidecar and
/// `SPIRA_GATE_EJECTED_SUITES` write it: comma or whitespace separated. Words in order,
/// each once, empties dropped. Names are not validated here; see [`is_suite_name`].
pub fn split_list(list: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in list.split(|c: char| c == ',' || c.is_whitespace()) {
        if !w.is_empty() && !out.iter().any(|x| x == w) {
            out.push(w.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_one_safe_component() {
        assert!(is_suite_name("test-gate-touched.sh"));
        assert!(!is_suite_name("test-.sh"));
        assert!(!is_suite_name("spira/test-a.sh"));
        assert!(!is_suite_name("test-../x.sh"));
        assert!(!is_suite_name("test-a;b.sh"));
        assert!(!is_suite_name("a.sh"));
    }

    #[test]
    fn lists_split_on_commas_and_blanks_once_each() {
        assert_eq!(split_list("a.sh,b.sh c.sh\n,a.sh"), ["a.sh", "b.sh", "c.sh"]);
        assert!(split_list(" ,\n").is_empty());
    }
}
