//! A bash lexer good enough to recover simple commands.
//!
//! Not a parser of the whole grammar: it recovers each simple command's words (raw text,
//! start line, the parameters the word itself expands) and its redirections, including the
//! commands nested in `$(…)`, backticks and `<(…)`. Comments are dropped; heredoc bodies
//! are data and are skipped. See DESIGN.md for the known limits.

/// One shell word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// Source text, quotes included.
    pub raw: String,
    /// 1-based line the word starts on.
    pub line: usize,
    /// Parameters this word expands at its own level (`$x`, `${x…}`, inside double quotes
    /// too) — not those of a nested `$(…)`, which belong to the nested command.
    pub vars: Vec<String>,
}

impl Word {
    /// `NAME` of a `NAME=value` / `NAME+=value` word, if it is one.
    pub fn assignment_name(&self) -> Option<&str> {
        let b = self.raw.as_bytes();
        if b.is_empty() || !(b[0].is_ascii_alphabetic() || b[0] == b'_') {
            return None;
        }
        let mut i = 1;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
            i += 1;
        }
        let end = i;
        if i < b.len() && b[i] == b'+' {
            i += 1;
        }
        (i < b.len() && b[i] == b'=').then(|| &self.raw[..end])
    }

    /// The word with quotes and backslashes removed — meaningful for a literal word only.
    pub fn unquoted(&self) -> String {
        self.raw.chars().filter(|c| !matches!(c, '\'' | '"' | '\\')).collect()
    }
}

/// A redirection: its operator and its operand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub op: String,
    pub target: Option<Word>,
}

/// One simple command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Command {
    pub words: Vec<Word>,
    pub redirects: Vec<Redirect>,
}

const RESERVED_HEAD: &[&str] = &["if", "then", "else", "elif", "do", "while", "until", "!", "{", "time"];

impl Command {
    /// The words after leading reserved words: `then python3 …` is a python3 command.
    pub fn body(&self) -> &[Word] {
        let mut i = 0;
        while i < self.words.len() && RESERVED_HEAD.contains(&self.words[i].raw.as_str()) {
            i += 1;
        }
        &self.words[i..]
    }

    /// Leading `NAME=value` words (the command's environment), and the rest (its argv).
    pub fn split_env(&self) -> (&[Word], &[Word]) {
        let body = self.body();
        let n = body.iter().take_while(|w| w.assignment_name().is_some()).count();
        body.split_at(n)
    }
}

/// Every simple command in `src`, nested ones included, in no particular order.
pub fn parse(src: &[u8]) -> Vec<Command> {
    let mut lx = Lexer { s: src, i: 0, line: 1, heredocs: Vec::new(), out: Vec::new() };
    lx.list(false);
    lx.out
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
    line: usize,
    heredocs: Vec<(Vec<u8>, bool)>,
    out: Vec<Command>,
}

fn is_meta(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>')
}

impl<'a> Lexer<'a> {
    fn peek(&self, k: usize) -> u8 {
        *self.s.get(self.i + k).unwrap_or(&0)
    }

    fn flush(&mut self, cmd: &mut Command) {
        if !cmd.words.is_empty() || !cmd.redirects.is_empty() {
            self.out.push(std::mem::take(cmd));
        }
    }

    /// A command list, until EOF or (when `in_paren`) the `)` closing a `$(` / `<(`.
    fn list(&mut self, in_paren: bool) {
        let mut cmd = Command::default();
        let mut depth = 0usize;
        let mut case_depth = 0usize;
        while self.i < self.s.len() {
            let c = self.s[self.i];
            match c {
                b' ' | b'\t' | b'\r' => self.i += 1,
                b'\\' if self.peek(1) == b'\n' => {
                    self.i += 2;
                    self.line += 1;
                }
                b'\n' => {
                    self.flush(&mut cmd);
                    self.i += 1;
                    self.line += 1;
                    self.read_heredocs();
                }
                b'#' => {
                    while self.i < self.s.len() && self.s[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                b'&' if self.peek(1) == b'>' => self.redirect(&mut cmd),
                b';' | b'&' | b'|' => {
                    self.flush(&mut cmd);
                    while matches!(self.peek(0), b';' | b'&' | b'|') {
                        self.i += 1;
                    }
                }
                b'(' if self.i > 0 && self.s[self.i - 1] == b'=' => {
                    self.i += 1;
                    while self.i < self.s.len() && self.s[self.i] != b')' {
                        match self.s[self.i] {
                            b'\n' => {
                                self.line += 1;
                                self.i += 1;
                            }
                            b' ' | b'\t' => self.i += 1,
                            _ => {
                                let from = self.i;
                                self.word();
                                if self.i == from {
                                    self.i += 1;
                                }
                            }
                        }
                    }
                    self.i += 1;
                }
                b'(' => {
                    self.flush(&mut cmd);
                    depth += 1;
                    self.i += 1;
                }
                b')' => {
                    self.flush(&mut cmd);
                    self.i += 1;
                    if depth > 0 {
                        depth -= 1;
                    } else if case_depth == 0 && in_paren {
                        return;
                    }
                    // otherwise: a case pattern's terminator, or a stray paren
                }
                b'<' | b'>' if self.peek(1) == b'(' => {
                    let w = self.word();
                    cmd.words.push(w);
                }
                b'<' | b'>' => self.redirect(&mut cmd),
                _ => {
                    let w = self.word();
                    // `2>&1`: a bare number glued to a redirection is its fd, not a word.
                    if matches!(self.peek(0), b'<' | b'>')
                        && !w.raw.is_empty()
                        && w.raw.bytes().all(|b| b.is_ascii_digit())
                    {
                        continue;
                    }
                    if cmd.body().is_empty() {
                        match w.raw.as_str() {
                            "case" => case_depth += 1,
                            "esac" => case_depth = case_depth.saturating_sub(1),
                            _ => {}
                        }
                    }
                    cmd.words.push(w);
                }
            }
        }
        self.flush(&mut cmd);
    }

    fn read_heredocs(&mut self) {
        for (delim, strip) in std::mem::take(&mut self.heredocs) {
            while self.i < self.s.len() {
                let start = self.i;
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
                let mut l = &self.s[start..self.i];
                if self.i < self.s.len() {
                    self.i += 1;
                    self.line += 1;
                }
                if strip {
                    while l.first() == Some(&b'\t') {
                        l = &l[1..];
                    }
                }
                let l = l.strip_suffix(b"\r").unwrap_or(l);
                if l == delim.as_slice() {
                    break;
                }
            }
        }
    }

    fn redirect(&mut self, cmd: &mut Command) {
        const OPS: &[&str] = &["&>>", "&>", "<<<", "<<-", "<<", "<>", "<&", ">>", ">&", ">|", "<", ">"];
        let rest = &self.s[self.i..];
        let op = OPS.iter().find(|o| rest.starts_with(o.as_bytes())).copied().unwrap_or(">");
        self.i += op.len();
        let op = op.to_string();
        while matches!(self.peek(0), b' ' | b'\t') {
            self.i += 1;
        }
        let procsub = matches!(self.peek(0), b'<' | b'>') && self.peek(1) == b'(';
        let target = if self.i < self.s.len() && (!is_meta(self.peek(0)) || procsub) {
            Some(self.word())
        } else {
            None
        };
        if op == "<<" || op == "<<-" {
            if let Some(t) = &target {
                self.heredocs.push((t.unquoted().into_bytes(), op == "<<-"));
            }
        }
        cmd.redirects.push(Redirect { op, target });
    }

    /// One word, up to the next unquoted metacharacter.
    fn word(&mut self) -> Word {
        let start = self.i;
        let line = self.line;
        let mut vars = Vec::new();
        while self.i < self.s.len() {
            let c = self.s[self.i];
            match c {
                b'\\' => {
                    if self.peek(1) == b'\n' {
                        self.line += 1;
                    }
                    self.i += 2;
                }
                b'\'' => self.squote(),
                b'"' => self.dquote(&mut vars),
                b'$' => self.dollar(&mut vars, false),
                b'`' => self.backtick(),
                b'<' | b'>' if self.peek(1) == b'(' => {
                    self.i += 2;
                    self.list(true);
                }
                _ if is_meta(c) => break,
                _ => self.i += 1,
            }
        }
        let end = self.i.min(self.s.len());
        Word { raw: String::from_utf8_lossy(&self.s[start..end]).into_owned(), line, vars }
    }

    fn squote(&mut self) {
        self.i += 1;
        while self.i < self.s.len() && self.s[self.i] != b'\'' {
            if self.s[self.i] == b'\n' {
                self.line += 1;
            }
            self.i += 1;
        }
        self.i += 1;
    }

    fn dquote(&mut self, vars: &mut Vec<String>) {
        self.i += 1;
        while self.i < self.s.len() {
            match self.s[self.i] {
                b'"' => {
                    self.i += 1;
                    return;
                }
                b'\\' => {
                    if self.peek(1) == b'\n' {
                        self.line += 1;
                    }
                    self.i += 2;
                }
                b'$' => self.dollar(vars, true),
                b'`' => self.backtick(),
                b'\n' => {
                    self.line += 1;
                    self.i += 1;
                }
                _ => self.i += 1,
            }
        }
    }

    fn dollar(&mut self, vars: &mut Vec<String>, in_dq: bool) {
        match self.peek(1) {
            b'\'' if !in_dq => {
                // $'…' — backslash escapes, including \'
                self.i += 2;
                while self.i < self.s.len() && self.s[self.i] != b'\'' {
                    if self.s[self.i] == b'\\' {
                        self.i += 1;
                    }
                    if self.peek(0) == b'\n' {
                        self.line += 1;
                    }
                    self.i += 1;
                }
                self.i += 1;
            }
            b'(' if self.peek(2) == b'(' => {
                // $(( arithmetic ))
                self.i += 3;
                let mut depth = 2usize;
                while self.i < self.s.len() && depth > 0 {
                    match self.s[self.i] {
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        b'\n' => self.line += 1,
                        _ => {}
                    }
                    self.i += 1;
                }
            }
            b'(' => {
                self.i += 2;
                self.list(true);
            }
            b'{' => {
                self.i += 2;
                self.param(vars, in_dq);
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                self.i += 1;
                let st = self.i;
                while self.peek(0).is_ascii_alphanumeric() || self.peek(0) == b'_' {
                    self.i += 1;
                }
                vars.push(String::from_utf8_lossy(&self.s[st..self.i]).into_owned());
            }
            c if c.is_ascii_digit() || b"@*#?$!-".contains(&c) => self.i += 2,
            _ => self.i += 1,
        }
    }

    /// `${…}` after the `${`: the parameter's name, then its operator text, which may itself
    /// expand parameters and contain quotes and substitutions.
    fn param(&mut self, vars: &mut Vec<String>, in_dq: bool) {
        if matches!(self.peek(0), b'#' | b'!') && self.peek(1) != b'}' {
            self.i += 1;
        }
        let st = self.i;
        while self.peek(0).is_ascii_alphanumeric() || self.peek(0) == b'_' {
            self.i += 1;
        }
        if self.i > st && !self.s[st].is_ascii_digit() {
            vars.push(String::from_utf8_lossy(&self.s[st..self.i]).into_owned());
        }
        while self.i < self.s.len() {
            match self.s[self.i] {
                b'}' => {
                    self.i += 1;
                    return;
                }
                b'\\' => self.i += 2,
                b'\'' if !in_dq => self.squote(),
                b'"' => self.dquote(vars),
                b'$' => self.dollar(vars, in_dq),
                b'`' => self.backtick(),
                b'\n' => {
                    self.line += 1;
                    self.i += 1;
                }
                _ => self.i += 1,
            }
        }
    }

    /// `` `…` ``: the content, unescaped, lexed on its own from the current line.
    fn backtick(&mut self) {
        self.i += 1;
        let line = self.line;
        let mut inner = Vec::new();
        while self.i < self.s.len() && self.s[self.i] != b'`' {
            let c = self.s[self.i];
            if c == b'\\' && matches!(self.peek(1), b'`' | b'\\' | b'$') {
                inner.push(self.peek(1));
                self.i += 2;
                continue;
            }
            if c == b'\n' {
                self.line += 1;
            }
            inner.push(c);
            self.i += 1;
        }
        self.i += 1;
        let mut sub = Lexer { s: &inner, i: 0, line, heredocs: Vec::new(), out: Vec::new() };
        sub.list(false);
        self.out.extend(sub.out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(c: &Command) -> Vec<String> {
        c.split_env().1.iter().map(|w| w.raw.clone()).collect()
    }

    fn find<'c>(cs: &'c [Command], head: &str) -> &'c Command {
        cs.iter()
            .find(|c| c.split_env().1.first().map(|w| w.raw.as_str()) == Some(head))
            .unwrap_or_else(|| panic!("no {head} command in {cs:#?}"))
    }

    #[test]
    fn multiline_single_quoted_script_is_one_word() {
        let cs = parse(b"python3 -c '\nimport sys\nprint(1)\n' \"$a_json\" x\necho done\n");
        let py = find(&cs, "python3");
        assert_eq!(argv(py).len(), 5);
        assert_eq!(py.words[3].line, 4);
        assert_eq!(py.words[3].vars, vec!["a_json"]);
        find(&cs, "echo");
    }

    #[test]
    fn env_prefix_and_reserved_words() {
        let cs = parse(b"if X_JSON=\"$y\" python3 -c 'p'; then :; fi\n");
        let py = find(&cs, "python3");
        let (env, _) = py.split_env();
        assert_eq!(env[0].assignment_name(), Some("X_JSON"));
    }

    #[test]
    fn command_substitution_is_its_own_command() {
        let cs = parse(b"x_json=\"$(FOO=1 python3 -c 'p' \"$b\")\"\n");
        let outer = cs.iter().find(|c| c.words[0].raw.starts_with("x_json=")).unwrap();
        assert!(outer.words[0].vars.is_empty(), "nested $b is not the outer word's");
        let py = find(&cs, "python3");
        assert_eq!(py.split_env().0[0].assignment_name(), Some("FOO"));
        assert_eq!(py.words.last().unwrap().vars, vec!["b"]);
    }

    #[test]
    fn escaped_brace_in_a_parameter_default_does_not_open_a_quote() {
        let cs = parse(b"a=\"${4:-{\\}}\"\n# `bd ready` note\nls\n");
        assert_eq!(cs.iter().filter_map(|c| argv(c).first().cloned()).collect::<Vec<_>>(), vec!["ls"]);
    }

    #[test]
    fn an_array_literal_is_not_a_command() {
        let cs = parse(b"P=(podman\n   git curl)\nls\n");
        assert_eq!(cs.iter().filter_map(|c| argv(c).first().cloned()).collect::<Vec<_>>(), vec!["ls"]);
    }

    #[test]
    fn heredoc_bodies_are_skipped_and_lines_counted() {
        let src = b"cat <<'EOF' | python3 -c 'x'\nX_JSON=\"$a\" python3 -c 'p'\nEOF\njq . \"$z\"\n";
        let cs = parse(src);
        assert_eq!(cs.iter().filter(|c| argv(c).first().map(String::as_str) == Some("python3")).count(), 1);
        let jq = find(&cs, "jq");
        assert_eq!(jq.words[0].line, 4);
    }

    #[test]
    fn heredoc_inside_command_substitution() {
        let src = b"P=\"$(cat <<'AWK'\n{ print $1 }\nAWK\n)\"\nawk \"$P\" f\n";
        let cs = parse(src);
        let awk = find(&cs, "awk");
        assert_eq!(awk.words[0].line, 5);
    }

    #[test]
    fn redirections_and_fds() {
        let cs = parse(b"jq . <<< \"$x_json\" 2>&1 >> \"$SPIRA_TOML\"\n");
        let jq = find(&cs, "jq");
        assert_eq!(argv(jq), vec!["jq", "."]);
        let ops: Vec<&str> = jq.redirects.iter().map(|r| r.op.as_str()).collect();
        assert_eq!(ops, vec!["<<<", ">&", ">>"]);
        assert_eq!(jq.redirects[2].target.as_ref().unwrap().vars, vec!["SPIRA_TOML"]);
    }

    #[test]
    fn comments_dropped_but_hash_inside_words_kept() {
        let cs = parse(b"# python3 -c 'x' \"$a_json\"\necho a#b ${#arr} # trailing\n");
        assert_eq!(cs.len(), 1);
        assert_eq!(argv(&cs[0]), vec!["echo", "a#b", "${#arr}"]);
        assert_eq!(cs[0].words[2].vars, vec!["arr"]);
    }

    #[test]
    fn param_default_expands_inner_vars_and_backticks_nest() {
        let cs = parse(b"f \"${a:-$b_json}\" `jq . \"$c\"`\n");
        let f = find(&cs, "f");
        assert_eq!(f.words[1].vars, vec!["a", "b_json"]);
        find(&cs, "jq");
    }

    #[test]
    fn case_patterns_inside_substitution() {
        let cs = parse(b"x=\"$(case $a in b) jq . \"$y\";; esac)\"\nawk 1\n");
        find(&cs, "jq");
        let awk = find(&cs, "awk");
        assert_eq!(awk.words[0].line, 2);
    }
}
