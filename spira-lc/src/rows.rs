//! SQL string escaping, the one escaper in the crate.

pub fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod escape_tests {
    use super::escape;

    #[test]
    fn hostile_text_stays_inside_its_literal() {
        let text = r"it's a \ ; DROP TABLE x; -- \' ''";
        let lit = format!("'{}'", escape(text));
        let mut chars = lit[1..lit.len() - 1].chars();
        let mut back = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => back.push(chars.next().expect("a backslash escapes the next char")),
                '\'' => panic!("unescaped quote closes the literal in {lit}"),
                c => back.push(c),
            }
        }
        assert_eq!(back, text);
    }

    #[test]
    fn only_one_escaper_exists_in_the_crate() {
        let dir = testkit::manifest_dir().join("src");
        let needle = ["replace('", "\\\\'", "'"].concat();
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("rows.rs") {
                let t = std::fs::read_to_string(&p).unwrap();
                assert!(!t.contains(&needle), "{} hand-rolls SQL escaping; use rows::escape", p.display());
            }
        }
    }
}
