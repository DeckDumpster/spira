//! In-process reader for the `NAME=value` subset a `.rune` file uses; any other construct is refused.

use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Bare,
    Double,
    Brace,
    Arith,
}

struct Ctx<'a> {
    vars: &'a HashMap<String, String>,
    env: &'a dyn Fn(&str) -> Option<String>,
}

impl Ctx<'_> {
    fn get(&self, name: &str) -> String {
        self.vars.get(name).cloned().or_else(|| (self.env)(name)).unwrap_or_default()
    }
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn read_name(c: &[char], i: &mut usize) -> String {
    let s = *i;
    while *i < c.len() && (c[*i].is_ascii_alphanumeric() || c[*i] == '_') {
        *i += 1;
    }
    c[s..*i].iter().collect()
}

fn dollar(c: &[char], i: &mut usize, ctx: &Ctx) -> Result<String, String> {
    *i += 1;
    match c.get(*i) {
        Some(&ch) if is_name_start(ch) => Ok(ctx.get(&read_name(c, i))),
        Some('{') => {
            *i += 1;
            if !c.get(*i).copied().is_some_and(is_name_start) {
                return Err("unsupported parameter expansion".into());
            }
            let name = read_name(c, i);
            let val = ctx.get(&name);
            match c.get(*i) {
                Some('}') => {
                    *i += 1;
                    Ok(val)
                }
                Some(':') if matches!(c.get(*i + 1), Some('-') | Some('+')) => {
                    let plus = c[*i + 1] == '+';
                    *i += 2;
                    let w = word(c, i, Mode::Brace, ctx)?;
                    if c.get(*i) != Some(&'}') {
                        return Err("unterminated ${...}".into());
                    }
                    *i += 1;
                    Ok(if (plus && !val.is_empty()) || (!plus && val.is_empty()) { w } else if plus { String::new() } else { val })
                }
                _ => Err("unsupported parameter expansion".into()),
            }
        }
        Some('(') if c.get(*i + 1) == Some(&'(') => {
            *i += 2;
            let e = word(c, i, Mode::Arith, ctx)?;
            if c.get(*i) != Some(&')') || c.get(*i + 1) != Some(&')') {
                return Err("unterminated $(( ))".into());
            }
            *i += 2;
            arith(&e).map(|n| n.to_string())
        }
        _ => Err("unsupported use of $".into()),
    }
}

fn arith(e: &str) -> Result<i64, String> {
    let mut total: i64 = 0;
    let mut sign = 1;
    let mut want_num = true;
    for tok in e.split_whitespace() {
        match (want_num, tok) {
            (false, "+") => (sign, want_num) = (1, true),
            (false, "-") => (sign, want_num) = (-1, true),
            (true, t) => {
                let n: i64 = t.parse().map_err(|_| format!("arithmetic operand {t:?} is not an integer"))?;
                total += sign * n;
                want_num = false;
            }
            (false, t) => return Err(format!("unsupported arithmetic operator {t:?}")),
        }
    }
    if want_num {
        return Err("incomplete arithmetic expression".into());
    }
    Ok(total)
}

fn word(c: &[char], i: &mut usize, mode: Mode, ctx: &Ctx) -> Result<String, String> {
    let mut out = String::new();
    while let Some(&ch) = c.get(*i) {
        match ch {
            '$' => out.push_str(&dollar(c, i, ctx)?),
            '"' if mode == Mode::Double => return Ok(out),
            '}' if mode == Mode::Brace => return Ok(out),
            ')' if mode == Mode::Arith => return Ok(out),
            c2 if mode == Mode::Bare && c2.is_whitespace() => return Ok(out),
            '"' if mode == Mode::Bare => {
                *i += 1;
                out.push_str(&word(c, i, Mode::Double, ctx)?);
                if c.get(*i) != Some(&'"') {
                    return Err("unterminated double quote".into());
                }
                *i += 1;
            }
            '\'' if mode == Mode::Bare => {
                let s = *i + 1;
                let Some(len) = c[s..].iter().position(|&x| x == '\'') else {
                    return Err("unterminated single quote".into());
                };
                out.extend(&c[s..s + len]);
                *i = s + len + 1;
            }
            '`' | '\\' => return Err(format!("unsupported character {ch:?}")),
            '\'' | '"' | '*' | '?' | '[' | '~' | '!' | '(' | ')' | '<' | '>' | '|' | '&' | ';' | '{' | '}' if mode != Mode::Double => {
                return Err(format!("unsupported character {ch:?}"));
            }
            _ => {
                out.push(ch);
                *i += 1;
            }
        }
    }
    match mode {
        Mode::Bare => Ok(out),
        _ => Err("unterminated expression".into()),
    }
}

/// Every assignment in `text`, later ones overriding earlier. `env` answers names the file did not assign.
pub fn parse(file: &str, text: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<HashMap<String, String>, String> {
    let mut vars = HashMap::new();
    for (n, line) in text.lines().enumerate() {
        let fail = |m: String| format!("{file}:{}: {m}", n + 1);
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let c: Vec<char> = t.chars().collect();
        if !is_name_start(c[0]) {
            return Err(fail("not a NAME=value assignment".into()));
        }
        let mut i = 0;
        let name = read_name(&c, &mut i);
        if c.get(i) != Some(&'=') {
            return Err(fail("not a NAME=value assignment".into()));
        }
        i += 1;
        let val = {
            let ctx = Ctx { vars: &vars, env };
            word(&c, &mut i, Mode::Bare, &ctx).map_err(fail)?
        };
        let rest: String = c[i..].iter().collect();
        let rest = rest.trim();
        if !rest.is_empty() && !rest.starts_with('#') {
            return Err(fail("trailing text after the value".into()));
        }
        vars.insert(name, val);
    }
    Ok(vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(n: &str) -> Option<String> {
        match n {
            "SCOPE" => Some("s".into()),
            "EMPTY" => Some(String::new()),
            _ => None,
        }
    }

    fn one(text: &str) -> Result<String, String> {
        parse("f.rune", text, &env).map(|m| m.get("X").cloned().unwrap_or_default())
    }

    #[test]
    fn the_subset_expands() {
        assert_eq!(one("X=a-b.c").unwrap(), "a-b.c");
        assert_eq!(one("X=\"${SCOPE:+$SCOPE,}$EMPTY\"").unwrap(), "s,");
        assert_eq!(one("X=\"${EMPTY:+y}${NOPE:-d}\"").unwrap(), "d");
        assert_eq!(one("X=$(( ${NOPE:-300} + 120 ))").unwrap(), "420");
        assert_eq!(one("A=1\nX=\"$A$A\" # note").unwrap(), "11");
        assert_eq!(one("X='$SCOPE'").unwrap(), "$SCOPE");
        assert_eq!(one("X=").unwrap(), "");
    }

    #[test]
    fn anything_outside_the_subset_is_refused_with_file_and_line() {
        for bad in ["X=$(date)", "X=`date`", "export X=1", "X=a;b", "X=\"a", "X=${A#b}", "X=a\\b", "X=*", "X=a b", "if true; then"] {
            let e = one(&format!("# c\n{bad}")).unwrap_err();
            assert!(e.starts_with("f.rune:2: "), "{bad}: {e}");
        }
    }
}
