//! `py-fstring-compat` — Python in suites uses no f-string form that only 3.12+ parses.
//! The round VM's python3 is older than the host's; a backslash inside an f-string's braces
//! or a quote of the string's own kind inside them passes every host check and fails first
//! in a round.

use crate::{lines, Entry, Finding, LintError, Rule, Tree};

pub struct PyFstringCompat;

const NAME: &str = "py-fstring-compat";

/// The 3.12-only constructs found in one line, as messages.
fn offences(line: &[u8]) -> Vec<&'static str> {
    let mut out = Vec::new();
    let n = line.len();
    let mut i = 0;
    while i < n {
        let prefixed = matches!(line[i], b'f' | b'F' | b'r' | b'R')
            && (i == 0 || !(line[i - 1].is_ascii_alphanumeric() || matches!(line[i - 1], b'_' | b'$' | b'-')));
        if !prefixed {
            i += 1;
            continue;
        }
        let mut j = i;
        let mut is_f = false;
        while j < n && matches!(line[j], b'f' | b'F' | b'r' | b'R' | b'b' | b'B') && j - i < 3 {
            is_f |= matches!(line[j], b'f' | b'F');
            j += 1;
        }
        if !is_f || j >= n || !matches!(line[j], b'\'' | b'"') {
            i += 1;
            continue;
        }
        let q = line[j];
        let triple = line[j..].starts_with(&[q, q, q]);
        j += if triple { 3 } else { 1 };
        let mut depth = 0usize;
        while j < n {
            let c = line[j];
            if depth == 0 {
                if c == b'\\' {
                    j += 2;
                    continue;
                }
                if c == q && (!triple || line[j..].starts_with(&[q, q, q])) {
                    j += if triple { 3 } else { 1 };
                    break;
                }
                if c == b'{' {
                    if line.get(j + 1) == Some(&b'{') {
                        j += 2;
                        continue;
                    }
                    depth = 1;
                }
            } else {
                match c {
                    b'\\' => out.push("backslash inside f-string braces"),
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ if c == q => out.push("f-string reuses its own quote inside braces"),
                    _ => {}
                }
            }
            j += 1;
        }
        i = j.max(i + 1);
    }
    out.dedup();
    out
}

impl Rule for PyFstringCompat {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        let Some(rest) = e.path.strip_prefix("spira/") else { return false };
        (rest.starts_with("test-") && rest.ends_with(".sh")) || rest.ends_with(".py")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let mut out = Vec::new();
        for e in crate::scope(tree, self)? {
            let Some(c) = tree.content(e) else { continue };
            for (i, l) in lines(c).iter().enumerate() {
                for m in offences(l) {
                    out.push(Finding {
                        rule: NAME,
                        path: e.path.clone(),
                        line: Some(i + 1),
                        message: format!("{m} — Python before 3.12 rejects it; build the value in a variable first"),
                    });
                }
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "The host's python3 accepts f-strings the round VM's rejects (PEP 701), so such a suite is green \
on every host check and red only in a round. Compute the expression before the f-string."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(PyFstringCompat)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> usize {
        offences(s.as_bytes()).len()
    }

    #[test]
    fn planted_offenders_are_caught() {
        assert_eq!(n(r#"print(f"{'\n'.join(xs)}")"#), 1);
        assert_eq!(n(r#"print(f"{d["k"]}")"#), 1);
        assert_eq!(n(r#"x = rf'{a}{b!r:{w}}{"\t".join(c)}'"#), 1);
    }

    #[test]
    fn pre_3_12_safe_forms_are_silent() {
        assert_eq!(n(r#"print(f"{d['k']} {{literal}} \n done {x:>{w}}")"#), 0);
        assert_eq!(n(r#"s = "\n".join(xs); print(f"{s}")"#), 0);
        assert_eq!(n(r#"print("{'\n'}", rf"\d{n}")"#), 0);
        assert_eq!(n(r#"[ -f "$f" ] && { printf 'x\n' > "$f"; }"#), 0);
    }

    #[test]
    fn an_empty_scope_is_refused() {
        let t = crate::testutil::TempDir::new("pyf-empty");
        let tree = Tree::from_paths(t.path(), ["spira/a.sh"], std::iter::empty::<&str>());
        assert_eq!(PyFstringCompat.check(&tree), Err(LintError::EmptyScope));
    }
}
