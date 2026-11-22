//! Which active `test:<id>` rows a member claims: a change to the body of the test the row names.

use spira_lint::rust_test::{test_body, test_sites};

/// One side of a member's diff: the `.rs` files it touches, as (path, bytes).
pub type Files = [(String, Vec<u8>)];

fn body_of(files: &Files, id: &str) -> Option<Vec<u8>> {
    files.iter().find_map(|(_, src)| {
        let site = test_sites(src)
            .into_iter()
            .find(|s| s.id.as_deref() == Some(id))?;
        test_body(src, &site)
    })
}

/// The `test:<id>` rows among `rows` whose test's body differs between `base` and `tip`. A test
/// found on only one side (stamped or removed by the member) claims nothing, and a body that is
/// byte-identical wherever the fn moved or whatever it was renamed to claims nothing.
pub fn claimed_rows(rows: &[String], base: &Files, tip: &Files) -> Vec<String> {
    rows.iter()
        .filter(|row| {
            let Some(id) = row.strip_prefix("test:") else {
                return false;
            };
            matches!((body_of(base, id), body_of(tip, id)), (Some(b), Some(t)) if b != t)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(name: &str, body: &str) -> Vec<u8> {
        format!("#[test_id(t0aaaaa)]\n#[test]\nfn {name}() {{\n{body}\n}}\n#[test_id(t0bbbbb)]\n#[test]\nfn other() {{\n    other_body();\n}}\n").into_bytes()
    }
    fn file(path: &str, bytes: Vec<u8>) -> Vec<(String, Vec<u8>)> {
        vec![(path.into(), bytes)]
    }
    fn rows() -> Vec<String> {
        vec![
            "test:t0aaaaa".into(),
            "test:t0bbbbb".into(),
            "suite:test-x.sh".into(),
        ]
    }

    #[test_id(tclm001)]
    #[test]
    fn an_edited_line_inside_the_stamped_tests_body_claims_its_row() {
        let base = file("a.rs", src("t", "    assert_eq!(1, 2);"));
        let tip = file("a.rs", src("t", "    assert_eq!(1, 1);"));
        assert_eq!(claimed_rows(&rows(), &base, &tip), ["test:t0aaaaa"]);
    }

    #[test_id(tclm002)]
    #[test]
    fn a_rename_and_a_move_with_an_identical_body_claims_nothing() {
        let base = file("a.rs", src("t", "    assert_eq!(1, 2);"));
        let tip = file("sub/b.rs", src("renamed", "    assert_eq!(1, 2);"));
        assert_eq!(claimed_rows(&rows(), &base, &tip), Vec::<String>::new());
    }

    #[test_id(tclm003)]
    #[test]
    fn an_edit_to_a_different_test_claims_nothing() {
        let base = file("a.rs", src("t", "    x();"));
        let tip = file(
            "a.rs",
            String::from_utf8(src("t", "    x();"))
                .unwrap()
                .replace("other_body", "changed")
                .into_bytes(),
        );
        assert_eq!(claimed_rows(&rows(), &base, &tip), ["test:t0bbbbb"]);
        let only_a = file("a.rs", src("t", "    x();"));
        assert_eq!(
            claimed_rows(&["test:t0aaaaa".to_string()], &only_a, &tip),
            Vec::<String>::new()
        );
    }

    #[test_id(tclm004)]
    #[test]
    fn a_test_removed_or_newly_stamped_by_the_member_claims_nothing() {
        let with = file("a.rs", src("t", "    x();"));
        let without: Vec<(String, Vec<u8>)> = file("a.rs", b"fn nothing() {}\n".to_vec());
        assert_eq!(claimed_rows(&rows(), &with, &without), Vec::<String>::new());
        assert_eq!(claimed_rows(&rows(), &without, &with), Vec::<String>::new());
    }
}
