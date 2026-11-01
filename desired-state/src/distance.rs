//! A section's distance: a closed expression grammar over its own numeric status fields.
//! Numbers, field names, `+ − × /` (ASCII `- *` too), `max(a, b)`, `min(a, b)` and
//! parentheses. Nothing else parses, so a distance can never call out or read another section.

use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(f64),
    Field(String),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Max(Box<Expr>, Box<Expr>),
    Min(Box<Expr>, Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Plus,
    Minus,
    Times,
    Slash,
    Open,
    Close,
    Comma,
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut chars = src.char_indices().peekable();
    while let Some(&(i, c)) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '+' => {
                chars.next();
                out.push(Tok::Plus);
            }
            '-' | '−' => {
                chars.next();
                out.push(Tok::Minus);
            }
            '*' | '×' => {
                chars.next();
                out.push(Tok::Times);
            }
            '/' => {
                chars.next();
                out.push(Tok::Slash);
            }
            '(' => {
                chars.next();
                out.push(Tok::Open);
            }
            ')' => {
                chars.next();
                out.push(Tok::Close);
            }
            ',' => {
                chars.next();
                out.push(Tok::Comma);
            }
            c if c.is_ascii_digit() => {
                let mut end = i;
                while let Some(&(j, d)) = chars.peek() {
                    if d.is_ascii_digit() || d == '.' {
                        end = j + d.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                let text = &src[i..end];
                let n: f64 = text
                    .parse()
                    .map_err(|_| format!("{text:?} is not a number"))?;
                out.push(Tok::Num(n));
            }
            c if c.is_ascii_lowercase() || c == '_' => {
                let mut end = i;
                while let Some(&(j, d)) = chars.peek() {
                    if d.is_ascii_lowercase() || d.is_ascii_digit() || d == '_' {
                        end = j + 1;
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push(Tok::Ident(src[i..end].to_string()));
            }
            other => return Err(format!("{other:?} is outside the distance grammar")),
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.at).cloned();
        self.at += 1;
        t
    }

    fn expect(&mut self, want: Tok, what: &str) -> Result<(), String> {
        match self.next() {
            Some(t) if t == want => Ok(()),
            _ => Err(format!("expected {what}")),
        }
    }

    fn expr(&mut self) -> Result<Expr, String> {
        let mut lhs = self.term()?;
        loop {
            match self.peek() {
                Some(Tok::Plus) => {
                    self.next();
                    lhs = Expr::Add(Box::new(lhs), Box::new(self.term()?));
                }
                Some(Tok::Minus) => {
                    self.next();
                    lhs = Expr::Sub(Box::new(lhs), Box::new(self.term()?));
                }
                _ => return Ok(lhs),
            }
        }
    }

    fn term(&mut self) -> Result<Expr, String> {
        let mut lhs = self.factor()?;
        loop {
            match self.peek() {
                Some(Tok::Times) => {
                    self.next();
                    lhs = Expr::Mul(Box::new(lhs), Box::new(self.factor()?));
                }
                Some(Tok::Slash) => {
                    self.next();
                    lhs = Expr::Div(Box::new(lhs), Box::new(self.factor()?));
                }
                _ => return Ok(lhs),
            }
        }
    }

    fn factor(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Expr::Num(n)),
            Some(Tok::Open) => {
                let e = self.expr()?;
                self.expect(Tok::Close, "')'")?;
                Ok(e)
            }
            Some(Tok::Ident(name)) if name == "max" || name == "min" => {
                self.expect(Tok::Open, &format!("'(' after {name}"))?;
                let a = self.expr()?;
                self.expect(Tok::Comma, &format!("',' between {name}'s arguments"))?;
                let b = self.expr()?;
                self.expect(Tok::Close, &format!("')' closing {name}"))?;
                Ok(if name == "max" {
                    Expr::Max(Box::new(a), Box::new(b))
                } else {
                    Expr::Min(Box::new(a), Box::new(b))
                })
            }
            Some(Tok::Ident(name)) => Ok(Expr::Field(name)),
            Some(t) => Err(format!("unexpected {t:?}")),
            None => Err("the expression ends early".into()),
        }
    }
}

/// Parses `src`, refusing anything outside the grammar.
pub fn parse(src: &str) -> Result<Expr, String> {
    let mut p = Parser {
        toks: lex(src)?,
        at: 0,
    };
    let e = p.expr()?;
    match p.peek() {
        None => Ok(e),
        Some(t) => Err(format!("unexpected {t:?} after the expression")),
    }
}

impl Expr {
    /// Every status field the expression reads.
    pub fn fields(&self) -> BTreeSet<&str> {
        let mut out = BTreeSet::new();
        self.collect(&mut out);
        out
    }

    fn collect<'a>(&'a self, out: &mut BTreeSet<&'a str>) {
        match self {
            Expr::Num(_) => {}
            Expr::Field(f) => {
                out.insert(f);
            }
            Expr::Add(a, b)
            | Expr::Sub(a, b)
            | Expr::Mul(a, b)
            | Expr::Div(a, b)
            | Expr::Max(a, b)
            | Expr::Min(a, b) => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    /// The value with each field read through `field`; a missing field or a division by zero
    /// is an error, never a silent zero.
    pub fn eval(&self, field: &dyn Fn(&str) -> Option<f64>) -> Result<f64, String> {
        let two = |a: &Expr, b: &Expr| Ok::<_, String>((a.eval(field)?, b.eval(field)?));
        Ok(match self {
            Expr::Num(n) => *n,
            Expr::Field(f) => field(f).ok_or_else(|| format!("{f} has no numeric value"))?,
            Expr::Add(a, b) => {
                let (a, b) = two(a, b)?;
                a + b
            }
            Expr::Sub(a, b) => {
                let (a, b) = two(a, b)?;
                a - b
            }
            Expr::Mul(a, b) => {
                let (a, b) = two(a, b)?;
                a * b
            }
            Expr::Div(a, b) => {
                let (a, b) = two(a, b)?;
                if b == 0.0 {
                    return Err("division by zero".into());
                }
                a / b
            }
            Expr::Max(a, b) => {
                let (a, b) = two(a, b)?;
                a.max(b)
            }
            Expr::Min(a, b) => {
                let (a, b) = two(a, b)?;
                a.min(b)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields<'a>(pairs: &'a [(&'a str, f64)]) -> impl Fn(&str) -> Option<f64> + 'a {
        move |f| pairs.iter().find(|(k, _)| *k == f).map(|(_, v)| *v)
    }

    #[test]
    fn the_designs_example_parses_and_evaluates_with_precedence() {
        let e = parse("max(0, green_unlanded_minutes) / 5 + repeat_identity_open").unwrap();
        assert_eq!(
            e.fields().into_iter().collect::<Vec<_>>(),
            ["green_unlanded_minutes", "repeat_identity_open"]
        );
        let v = e
            .eval(&fields(&[("green_unlanded_minutes", 10.0), ("repeat_identity_open", 1.0)]))
            .unwrap();
        assert_eq!(v, 3.0);
    }

    #[test]
    fn unicode_and_ascii_operators_mean_the_same() {
        let f = fields(&[("a", 6.0), ("b", 2.0)]);
        let u = parse("a − b × 2").unwrap().eval(&f).unwrap();
        let a = parse("a - b * 2").unwrap().eval(&f).unwrap();
        assert_eq!((u, a), (2.0, 2.0));
        assert_eq!(parse("min(a, (b))").unwrap().eval(&f).unwrap(), 2.0);
        assert_eq!(parse("(a - b) / b").unwrap().eval(&f).unwrap(), 2.0);
    }

    #[test]
    fn nothing_outside_the_grammar_parses() {
        for bad in [
            "a ^ 2",
            "abs(a)",
            "a > 1",
            "max(a)",
            "max(a, b",
            "a b",
            "",
            "-a",
            "a +",
            "A",
            "1.2.3",
            "(a",
            "max a, b)",
            "min(a b)",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn a_missing_field_or_a_zero_divisor_is_an_error() {
        let f = fields(&[("a", 1.0), ("z", 0.0)]);
        assert!(parse("a + b").unwrap().eval(&f).is_err());
        assert!(parse("a / z").unwrap().eval(&f).is_err());
    }
}
