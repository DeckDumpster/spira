//! The test-id grammar every consumer shares, and the bead-id shape a row may name.

/// A bead id a row may name: a plain ref component (`spira/<id>`).
pub fn valid_bead_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with(['-', '.'])
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub use spira_config::unit_tier::valid_test_id;

/// Ok if `id` is `suite:<file>` or `test:<id>`.
pub fn check_test_id(id: &str) -> Result<(), String> {
    let token = |s: &str| !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c.is_control());
    if id.starts_with("unit:") || id.starts_with("integration:") {
        return Err(format!("unknown test id {id:?}: unit: and integration: rows are retired: name the test by its id, test:<id>, the last segment of its listed name"));
    }
    match (id.strip_prefix("suite:"), id.strip_prefix("test:")) {
        (Some(file), _) if token(file) => Ok(()),
        (_, Some(t)) if valid_test_id(t) => Ok(()),
        _ => Err(format!("unknown test id {id:?}: want suite:<file> or test:<id>")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tqccdgg)]
    #[test]
    fn the_two_shapes_are_accepted() {
        for ok in ["suite:test-foo.sh", "suite:x.sh", "test:t0aaaaa"] {
            assert!(check_test_id(ok).is_ok(), "{ok}");
        }
    }

    #[test_id(tgwfflh)]
    #[test]
    fn other_shapes_are_refused() {
        for bad in ["", "foo", "suite:", "suite:a b", "suite:a\tb", "test:T0", "test:", "test:t0aaaaaa", "test:t0aaaa", "test:t0aaaa-", "other:x y"] {
            assert!(check_test_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test_id(tr1e2d3)]
    #[test]
    fn name_keyed_rows_are_retired_with_the_fix_named() {
        for bad in ["unit:a b", "integration:p x", "unit:loom a::b::t0aaaaa"] {
            let e = check_test_id(bad).unwrap_err();
            assert!(e.contains("unit: and integration: rows are retired: name the test by its id, test:<id>, the last segment of its listed name"), "{e}");
        }
    }

    #[test_id(t5iy9xd)]
    #[test]
    fn bead_ids_are_plain_ref_components() {
        assert!(valid_bead_id("sp-zr8wi1") && valid_bead_id("a_b.c"));
        for bad in ["", "-a", ".a", "a..b", "a/b", "a b"] {
            assert!(!valid_bead_id(bad), "{bad:?}");
        }
    }
}
