//! The slice of jq `gh --jq` is called with in this tree, evaluated in-process: a world must not
//! depend on a `jq` binary, which the test container does not carry.
//!
//! Paths (`.a.b`, `.[0]`, `.[]`), pipes, `//`, `==`/`!=`, `and`/`or`, array construction,
//! parentheses, literals, and `length`, `select`, `map`, `join`, `startswith`, `not`.
//! Anything else is an error that names the unsupported token, never a silent empty answer.

use serde_json::Value;

type R<T> = Result<T, String>;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Dot,
    Ident(String),
    Str(String),
    Num(f64),
    LBracket,
    RBracket,
    LParen,
    RParen,
    Pipe,
    Alt,
    Eq,
    Ne,
    Comma,
}

fn lex(src: &str) -> R<Vec<Tok>> {
    let cs: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < cs.len() {
        let c = cs[i];
        match c {
            ' ' | '\t' | '\n' => i += 1,
            '.' => {
                out.push(Tok::Dot);
                i += 1;
            }
            '[' => {
                out.push(Tok::LBracket);
                i += 1;
            }
            ']' => {
                out.push(Tok::RBracket);
                i += 1;
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            '|' => {
                out.push(Tok::Pipe);
                i += 1;
            }
            '/' if cs.get(i + 1) == Some(&'/') => {
                out.push(Tok::Alt);
                i += 2;
            }
            '=' if cs.get(i + 1) == Some(&'=') => {
                out.push(Tok::Eq);
                i += 2;
            }
            '!' if cs.get(i + 1) == Some(&'=') => {
                out.push(Tok::Ne);
                i += 2;
            }
            '"' => {
                let mut s = String::new();
                i += 1;
                loop {
                    match cs.get(i) {
                        None => return Err("jq: unterminated string".into()),
                        Some('"') => {
                            i += 1;
                            break;
                        }
                        Some('\\') => {
                            let e = cs.get(i + 1).ok_or("jq: dangling backslash")?;
                            s.push(match e {
                                'n' => '\n',
                                't' => '\t',
                                other => *other,
                            });
                            i += 2;
                        }
                        Some(ch) => {
                            s.push(*ch);
                            i += 1;
                        }
                    }
                }
                out.push(Tok::Str(s));
            }
            c if c.is_ascii_digit() || c == '-' => {
                let start = i;
                i += 1;
                while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
                    i += 1;
                }
                let t: String = cs[start..i].iter().collect();
                out.push(Tok::Num(t.parse().map_err(|_| format!("jq: bad number {t:?}"))?));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < cs.len() && (cs[i].is_ascii_alphanumeric() || cs[i] == '_') {
                    i += 1;
                }
                out.push(Tok::Ident(cs[start..i].iter().collect()));
            }
            other => return Err(format!("jq: unsupported character {other:?} in {src:?}")),
        }
    }
    Ok(out)
}

#[derive(Debug, Clone)]
enum Ex {
    Identity,
    Field(Box<Ex>, String),
    Index(Box<Ex>, Box<Ex>),
    Iterate(Box<Ex>),
    Lit(Value),
    Pipe(Box<Ex>, Box<Ex>),
    Comma(Box<Ex>, Box<Ex>),
    Alt(Box<Ex>, Box<Ex>),
    Cmp(Box<Ex>, bool, Box<Ex>),
    And(Box<Ex>, Box<Ex>),
    Or(Box<Ex>, Box<Ex>),
    Collect(Option<Box<Ex>>),
    Call(String, Vec<Ex>),
}

struct P {
    t: Vec<Tok>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(s)) if s == w)
    }
    fn pipe(&mut self) -> R<Ex> {
        let mut l = self.comma()?;
        while self.eat(&Tok::Pipe) {
            l = Ex::Pipe(Box::new(l), Box::new(self.comma()?));
        }
        Ok(l)
    }
    fn comma(&mut self) -> R<Ex> {
        let mut l = self.alt()?;
        while self.eat(&Tok::Comma) {
            l = Ex::Comma(Box::new(l), Box::new(self.alt()?));
        }
        Ok(l)
    }
    fn alt(&mut self) -> R<Ex> {
        let l = self.or()?;
        if self.eat(&Tok::Alt) {
            return Ok(Ex::Alt(Box::new(l), Box::new(self.alt()?)));
        }
        Ok(l)
    }
    fn or(&mut self) -> R<Ex> {
        let mut l = self.and()?;
        while self.word("or") {
            self.i += 1;
            l = Ex::Or(Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }
    fn and(&mut self) -> R<Ex> {
        let mut l = self.cmp()?;
        while self.word("and") {
            self.i += 1;
            l = Ex::And(Box::new(l), Box::new(self.cmp()?));
        }
        Ok(l)
    }
    fn cmp(&mut self) -> R<Ex> {
        let l = self.postfix()?;
        let eq = if self.eat(&Tok::Eq) {
            true
        } else if self.eat(&Tok::Ne) {
            false
        } else {
            return Ok(l);
        };
        Ok(Ex::Cmp(Box::new(l), eq, Box::new(self.postfix()?)))
    }
    fn postfix(&mut self) -> R<Ex> {
        let mut e = self.primary()?;
        loop {
            if self.peek() == Some(&Tok::Dot) && matches!(self.t.get(self.i + 1), Some(Tok::Ident(_))) {
                self.i += 1;
                let Some(Tok::Ident(n)) = self.t.get(self.i).cloned() else { unreachable!() };
                self.i += 1;
                e = Ex::Field(Box::new(e), n);
            } else if self.peek() == Some(&Tok::LBracket) {
                e = self.bracket(e)?;
            } else {
                return Ok(e);
            }
        }
    }
    fn bracket(&mut self, base: Ex) -> R<Ex> {
        self.i += 1;
        if self.eat(&Tok::RBracket) {
            return Ok(Ex::Iterate(Box::new(base)));
        }
        let idx = self.pipe()?;
        if !self.eat(&Tok::RBracket) {
            return Err("jq: expected ]".into());
        }
        Ok(Ex::Index(Box::new(base), Box::new(idx)))
    }
    fn primary(&mut self) -> R<Ex> {
        match self.t.get(self.i).cloned() {
            Some(Tok::Dot) => {
                self.i += 1;
                match self.t.get(self.i) {
                    Some(Tok::Ident(n)) => {
                        let n = n.clone();
                        self.i += 1;
                        Ok(Ex::Field(Box::new(Ex::Identity), n))
                    }
                    Some(Tok::Str(s)) => {
                        let s = s.clone();
                        self.i += 1;
                        Ok(Ex::Field(Box::new(Ex::Identity), s))
                    }
                    _ => Ok(Ex::Identity),
                }
            }
            Some(Tok::Str(s)) => {
                self.i += 1;
                Ok(Ex::Lit(Value::String(s)))
            }
            Some(Tok::Num(n)) => {
                self.i += 1;
                Ok(Ex::Lit(if n.fract() == 0.0 { serde_json::json!(n as i64) } else { serde_json::json!(n) }))
            }
            Some(Tok::LParen) => {
                self.i += 1;
                let e = self.pipe()?;
                if !self.eat(&Tok::RParen) {
                    return Err("jq: expected )".into());
                }
                Ok(e)
            }
            Some(Tok::LBracket) => {
                self.i += 1;
                if self.eat(&Tok::RBracket) {
                    return Ok(Ex::Collect(None));
                }
                let e = self.pipe()?;
                if !self.eat(&Tok::RBracket) {
                    return Err("jq: expected ]".into());
                }
                Ok(Ex::Collect(Some(Box::new(e))))
            }
            Some(Tok::Ident(n)) => {
                self.i += 1;
                match n.as_str() {
                    "null" => return Ok(Ex::Lit(Value::Null)),
                    "true" => return Ok(Ex::Lit(Value::Bool(true))),
                    "false" => return Ok(Ex::Lit(Value::Bool(false))),
                    _ => {}
                }
                let mut args = Vec::new();
                if self.eat(&Tok::LParen) {
                    args.push(self.pipe()?);
                    if !self.eat(&Tok::RParen) {
                        return Err(format!("jq: expected ) after {n}("));
                    }
                }
                Ok(Ex::Call(n, args))
            }
            other => Err(format!("jq: unexpected {other:?}")),
        }
    }
}

fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false))
}

fn eval(e: &Ex, v: &Value) -> R<Vec<Value>> {
    Ok(match e {
        Ex::Identity => vec![v.clone()],
        Ex::Lit(x) => vec![x.clone()],
        Ex::Field(b, n) => {
            let mut out = Vec::new();
            for x in eval(b, v)? {
                out.push(match &x {
                    Value::Object(m) => m.get(n).cloned().unwrap_or(Value::Null),
                    Value::Null => Value::Null,
                    other => return Err(format!("jq: cannot index {} with \"{n}\"", kind(other))),
                });
            }
            out
        }
        Ex::Index(b, i) => {
            let mut out = Vec::new();
            for x in eval(b, v)? {
                for k in eval(i, v)? {
                    out.push(match (&x, &k) {
                        (Value::Array(a), Value::Number(n)) => {
                            let n = n.as_f64().unwrap_or(0.0) as i64;
                            let at = if n < 0 { a.len() as i64 + n } else { n };
                            usize::try_from(at).ok().and_then(|at| a.get(at)).cloned().unwrap_or(Value::Null)
                        }
                        (Value::Object(m), Value::String(s)) => m.get(s).cloned().unwrap_or(Value::Null),
                        (Value::Null, _) => Value::Null,
                        _ => return Err(format!("jq: cannot index {} with {}", kind(&x), kind(&k))),
                    });
                }
            }
            out
        }
        Ex::Iterate(b) => {
            let mut out = Vec::new();
            for x in eval(b, v)? {
                match x {
                    Value::Array(a) => out.extend(a),
                    Value::Object(m) => out.extend(m.into_iter().map(|(_, v)| v)),
                    other => return Err(format!("jq: cannot iterate over {}", kind(&other))),
                }
            }
            out
        }
        Ex::Pipe(a, b) => {
            let mut out = Vec::new();
            for x in eval(a, v)? {
                out.extend(eval(b, &x)?);
            }
            out
        }
        Ex::Comma(a, b) => {
            let mut out = eval(a, v)?;
            out.extend(eval(b, v)?);
            out
        }
        Ex::Alt(a, b) => {
            let l: Vec<Value> = eval(a, v).unwrap_or_default().into_iter().filter(truthy).collect();
            if l.is_empty() {
                eval(b, v)?
            } else {
                l
            }
        }
        Ex::Cmp(a, eq, b) => {
            let mut out = Vec::new();
            for y in eval(b, v)? {
                for x in eval(a, v)? {
                    out.push(Value::Bool((x == y) == *eq));
                }
            }
            out
        }
        Ex::And(a, b) => {
            let mut out = Vec::new();
            for x in eval(a, v)? {
                if !truthy(&x) {
                    out.push(Value::Bool(false));
                } else {
                    out.extend(eval(b, v)?.iter().map(|y| Value::Bool(truthy(y))));
                }
            }
            out
        }
        Ex::Or(a, b) => {
            let mut out = Vec::new();
            for x in eval(a, v)? {
                if truthy(&x) {
                    out.push(Value::Bool(true));
                } else {
                    out.extend(eval(b, v)?.iter().map(|y| Value::Bool(truthy(y))));
                }
            }
            out
        }
        Ex::Collect(inner) => match inner {
            None => vec![Value::Array(Vec::new())],
            Some(i) => vec![Value::Array(eval(i, v)?)],
        },
        Ex::Call(name, args) => call(name, args, v)?,
    })
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn call(name: &str, args: &[Ex], v: &Value) -> R<Vec<Value>> {
    let one = |i: usize| -> R<&Ex> { args.get(i).ok_or_else(|| format!("jq: {name} needs an argument")) };
    Ok(match name {
        "length" => vec![match v {
            Value::Null => serde_json::json!(0),
            Value::String(s) => serde_json::json!(s.chars().count()),
            Value::Array(a) => serde_json::json!(a.len()),
            Value::Object(m) => serde_json::json!(m.len()),
            other => return Err(format!("jq: {} has no length", kind(other))),
        }],
        "not" => vec![Value::Bool(!truthy(v))],
        "select" => {
            let mut out = Vec::new();
            for c in eval(one(0)?, v)? {
                if truthy(&c) {
                    out.push(v.clone());
                }
            }
            out
        }
        "map" => {
            let Value::Array(a) = v else { return Err(format!("jq: cannot map over {}", kind(v))) };
            let mut out = Vec::new();
            for x in a {
                out.extend(eval(one(0)?, x)?);
            }
            vec![Value::Array(out)]
        }
        "join" => {
            let Value::Array(a) = v else { return Err(format!("jq: cannot join {}", kind(v))) };
            let sep = match eval(one(0)?, v)?.first() {
                Some(Value::String(s)) => s.clone(),
                _ => return Err("jq: join takes a string".into()),
            };
            let parts: R<Vec<String>> = a
                .iter()
                .map(|x| match x {
                    Value::Null => Ok(String::new()),
                    Value::String(s) => Ok(s.clone()),
                    Value::Number(_) | Value::Bool(_) => Ok(x.to_string()),
                    other => Err(format!("jq: cannot join {}", kind(other))),
                })
                .collect();
            vec![Value::String(parts?.join(&sep))]
        }
        "startswith" => {
            let want = match eval(one(0)?, v)?.first() {
                Some(Value::String(s)) => s.clone(),
                _ => return Err("jq: startswith takes a string".into()),
            };
            match v {
                Value::String(s) => vec![Value::Bool(s.starts_with(&want))],
                other => return Err(format!("jq: startswith() requires string inputs, not {}", kind(other))),
            }
        }
        other => return Err(format!("jq: unsupported function {other}")),
    })
}

/// Evaluates `expr` over `json` and renders each output as `jq -r` does: strings raw,
/// everything else compact JSON, one per line.
pub fn jq_r(json: &str, expr: &str) -> R<String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("jq: input is not JSON: {e}"))?;
    let mut p = P { t: lex(expr)?, i: 0 };
    let ex = p.pipe()?;
    if p.i != p.t.len() {
        return Err(format!("jq: unexpected {:?} in {expr:?}", p.t[p.i]));
    }
    let mut out = String::new();
    for x in eval(&ex, &v)? {
        match x {
            Value::String(s) => out.push_str(&s),
            other => out.push_str(&other.to_string()),
        }
        out.push('\n');
    }
    Ok(out)
}
