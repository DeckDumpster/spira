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

/// Ok if `id` is `suite:<file>`, `unit:<binary> <test path>` or `integration:<package> <test path>`.
pub fn check_test_id(id: &str) -> Result<(), String> {
    let refuse = || {
        Err(format!(
            "unknown test id {id:?}: want suite:<file>, unit:<binary> <test path> or integration:<package> <test path>"
        ))
    };
    let token = |s: &str| !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c.is_control());
    if let Some(file) = id.strip_prefix("suite:") {
        return if token(file) { Ok(()) } else { refuse() };
    }
    for prefix in ["unit:", "integration:"] {
        if let Some(rest) = id.strip_prefix(prefix) {
            return match rest.split_once(' ') {
                Some((binary, test)) if token(binary) && token(test) => Ok(()),
                _ => refuse(),
            };
        }
    }
    refuse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tqccdgg)]
    #[test]
    fn the_three_shapes_are_accepted() {
        for ok in [
            "suite:test-foo.sh",
            "unit:loom factory::tests::a",
            "unit:gate::bin/gate tests::x",
            "integration:gate a::b",
        ] {
            assert!(check_test_id(ok).is_ok(), "{ok}");
        }
    }

    #[test_id(tgwfflh)]
    #[test]
    fn other_shapes_are_refused() {
        for bad in [
            "",
            "foo",
            "suite:",
            "suite:a b",
            "suite:a\tb",
            "unit:loom",
            "unit:loom  t",
            "unit: t",
            "unit:a b c",
            "unit:a b\n",
            "other:x y",
            "integration:p",
        ] {
            assert!(check_test_id(bad).is_err(), "{bad:?}");
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
