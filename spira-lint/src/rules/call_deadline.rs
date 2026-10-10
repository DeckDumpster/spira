//! `call-deadline` — a subprocess, socket or network call carries an explicit timeout, and
//! no explicit call timeout exceeds [`CAP_SECS`]. Contract and known limits: DESIGN.md.
//!
//! A batch job (gate, build, test suite, release verify) is exempt only where the call site
//! says so in a comment: `// batch-job: <why>` / `# batch-job: <why>`, on the call's line or
//! in the comment block directly above it (Rust: or above the enclosing `fn`). The marker is
//! read from comment bytes only, never from a string.
//!
//! Existing violations are counted per file in `spira-lint/call-deadline-allow`, which only
//! shrinks: a file over its count fails, and a file under it fails until the line is lowered.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust::{self, Class};
use crate::lex::shell;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct CallDeadline;

const NAME: &str = "call-deadline";
const ALLOW_FILE: &str = "spira-lint/call-deadline-allow";
const CAP_SECS: f64 = 5.0;

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

fn marker_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"batch-job:\s*\S")
}

pub struct Hit {
    pub line: usize,
    pub message: String,
}

fn hit(line: usize, message: impl Into<String>) -> Hit {
    Hit { line, message: message.into() }
}

fn over(secs: f64) -> String {
    format!("call timeout of {secs}s is above the {CAP_SECS}s cap")
}

/// `Some(true)` per line that carries a `batch-job:` marker in a comment.
fn marked_lines(src: &[u8], comment: impl Fn(usize) -> bool) -> Vec<bool> {
    let mut out = vec![false];
    let mut buf: Vec<u8> = Vec::new();
    let flush = |buf: &mut Vec<u8>, out: &mut Vec<bool>| {
        *out.last_mut().unwrap() = marker_re().is_match(buf);
        buf.clear();
    };
    for (i, &b) in src.iter().enumerate() {
        if b == b'\n' {
            flush(&mut buf, &mut out);
            out.push(false);
        } else if comment(i) {
            buf.push(b);
        }
    }
    flush(&mut buf, &mut out);
    out
}

/// Whether `line` (1-based) is marked: on it, or in the comment/attribute block directly above.
fn is_marked(lines: &[&[u8]], marked: &[bool], line: usize, skip: impl Fn(&[u8]) -> bool) -> bool {
    if marked.get(line - 1).copied().unwrap_or(false) {
        return true;
    }
    let mut l = line - 1;
    while l > 0 {
        let text = lines.get(l - 1).copied().unwrap_or(b"");
        if !skip(text) {
            break;
        }
        if marked.get(l - 1).copied().unwrap_or(false) {
            return true;
        }
        l -= 1;
    }
    false
}

fn trimmed(b: &[u8]) -> &[u8] {
    let s = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    &b[s..]
}

pub(crate) struct FnSpan {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

pub(crate) fn fn_spans(code: &[u8]) -> Vec<FnSpan> {
    static R: OnceLock<Regex> = OnceLock::new();
    let mut out = Vec::new();
    for m in re(&R, r"\bfn\s+[A-Za-z_]\w*").find_iter(code) {
        let mut i = m.end();
        let mut paren = 0i32;
        let mut open = None;
        while i < code.len() {
            match code[i] {
                b'(' | b'[' => paren += 1,
                b')' | b']' => paren -= 1,
                b';' if paren == 0 => break,
                b'{' if paren == 0 => {
                    open = Some(i);
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        let Some(o) = open else { continue };
        let mut depth = 0i32;
        let mut j = o;
        while j < code.len() {
            match code[j] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        out.push(FnSpan { start: m.start(), end: j.min(code.len()) });
    }
    out
}

pub(crate) fn enclosing(spans: &[FnSpan], at: usize) -> Option<&FnSpan> {
    spans.iter().filter(|s| s.start <= at && at < s.end).min_by_key(|s| s.end - s.start)
}

pub(crate) fn statement(code: &[u8], at: usize) -> &[u8] {
    let from = code[..at].iter().rposition(|b| matches!(b, b';' | b'{' | b'}')).map_or(0, |p| p + 1);
    let to = code[at..].iter().position(|b| *b == b';').map_or(code.len(), |p| at + p);
    &code[from..to]
}

fn secs_of(unit: &[u8], n: f64) -> f64 {
    match unit {
        b"from_millis" => n / 1000.0,
        b"from_mins" => n * 60.0,
        b"from_hours" => n * 3600.0,
        _ => n,
    }
}

/// Every call-deadline violation in one Rust source; `in_test` says whether a byte offset
/// is test code, which is exempt.
pub fn scan_rust(src: &[u8], in_test: &dyn Fn(usize) -> bool) -> Vec<Hit> {
    static CMD: OnceLock<Regex> = OnceLock::new();
    static EXEC: OnceLock<Regex> = OnceLock::new();
    static SOCK: OnceLock<Regex> = OnceLock::new();
    static BOUND: OnceLock<Regex> = OnceLock::new();
    static DUR: OnceLock<Regex> = OnceLock::new();
    static CALLISH: OnceLock<Regex> = OnceLock::new();
    static TIMEOUT_ARG: OnceLock<Regex> = OnceLock::new();
    static ATTR: OnceLock<Regex> = OnceLock::new();

    let cl = rust::classify(src);
    let code = cl.code_only();
    let kept = cl.without_comments();
    let spans = fn_spans(&code);
    let lines: Vec<&[u8]> = src.split(|b| *b == b'\n').collect();
    let marked = marked_lines(src, |i| cl.class[i] == Class::Comment);
    let skip_above = |t: &[u8]| {
        let t = trimmed(t);
        t.starts_with(b"//") || t.starts_with(b"/*") || t.starts_with(b"*") || re(&ATTR, r"^#!?\[").is_match(t)
    };
    let exempt = |at: usize, fn_at: Option<&FnSpan>| {
        let line = cl.line_of(at);
        is_marked(&lines, &marked, line, skip_above)
            || fn_at.is_some_and(|f| is_marked(&lines, &marked, cl.line_of(f.start), skip_above))
    };

    let mut out = Vec::new();
    let bound = re(&BOUND, r"(?i)timeout|deadline|bounded");

    for m in re(&CMD, r"\bCommand::new\s*\(").find_iter(&code) {
        let at = m.start();
        if in_test(at) {
            continue;
        }
        let f = enclosing(&spans, at);
        let body: &[u8] = f.map_or_else(|| statement(&code, at), |f| &code[f.start..f.end]);
        if !re(&EXEC, r"\.(output|status|spawn|exec)\s*\(\s*\)").is_match(body) {
            continue;
        }
        if exempt(at, f) {
            continue;
        }
        let tail = &kept[m.end()..kept.len().min(m.end() + 240)];
        if tail.trim_ascii_start().starts_with(b"\"timeout\"") {
            let arg = re(&TIMEOUT_ARG, r#""(\d+(?:\.\d+)?)([smhd]?)""#);
            if let Some(c) = arg.captures(tail) {
                let secs = duration_secs(&c[1], &c[2]);
                if secs > CAP_SECS {
                    out.push(hit(cl.line_of(at), over(secs)));
                }
            }
            continue;
        }
        if !bound.is_match(body) {
            out.push(hit(cl.line_of(at), "subprocess call without a timeout"));
        }
    }

    for m in re(&SOCK, r"\b(?:Tcp|Unix)Stream::connect\s*\(").find_iter(&code) {
        let at = m.start();
        if in_test(at) {
            continue;
        }
        let f = enclosing(&spans, at);
        let body: &[u8] = f.map_or_else(|| statement(&code, at), |f| &code[f.start..f.end]);
        if !bound.is_match(body) && !exempt(at, f) {
            out.push(hit(cl.line_of(at), "socket call without a timeout"));
        }
    }

    let dur = re(&DUR, r"Duration::(from_secs_f64|from_secs_f32|from_secs|from_millis|from_mins|from_hours)\s*\(\s*([0-9][0-9_]*(?:\.[0-9]+)?)");
    let callish = re(
        &CALLISH,
        r"(?i)timeout|deadline|\bCommand\b|\bconnect|\.output\(|\.status\(|\.spawn\(|\b(?:\w*_)?(?:run|exec|spawn|capture|bounded)(?:_\w+)?\s*\(",
    );
    for c in dur.captures_iter(&code) {
        let at = c.get(0).map_or(0, |m| m.start());
        if in_test(at) {
            continue;
        }
        let name = &c[1];
        let n: f64 = String::from_utf8_lossy(&c[2]).replace('_', "").parse().unwrap_or(0.0);
        let unit: &[u8] = if name == b"from_millis" || name == b"from_mins" || name == b"from_hours" { name } else { b"from_secs" };
        let secs = secs_of(unit, n);
        if secs > CAP_SECS && callish.is_match(statement(&code, at)) && !exempt(at, enclosing(&spans, at)) {
            out.push(hit(cl.line_of(at), over(secs)));
        }
    }
    out.sort_by_key(|h| h.line);
    out
}

fn duration_secs(n: &[u8], unit: &[u8]) -> f64 {
    let n: f64 = String::from_utf8_lossy(n).parse().unwrap_or(0.0);
    match unit {
        b"m" => n * 60.0,
        b"h" => n * 3600.0,
        b"d" => n * 86400.0,
        _ => n,
    }
}

const NET_PROGRAMS: &[&str] = &[
    "bd", "spira-bd", "dolt", "mysql", "psql", "curl", "wget", "gh", "ssh", "scp", "rsync", "nc", "ncat", "podman", "docker",
];
const NET_VARS: &[&str] = &["BD", "SPIRA_BD", "DOLT_BIN", "DOLT"];
const GIT_NET: &[&str] = &["fetch", "pull", "push", "clone", "ls-remote"];

fn parse_duration_word(w: &str) -> Option<f64> {
    let (digits, unit) = match w.char_indices().find(|(_, c)| !(c.is_ascii_digit() || *c == '.')) {
        Some((i, _)) => w.split_at(i),
        None => (w, ""),
    };
    let n: f64 = digits.parse().ok()?;
    match unit {
        "" | "s" => Some(n),
        "m" => Some(n * 60.0),
        "h" => Some(n * 3600.0),
        "d" => Some(n * 86400.0),
        _ => None,
    }
}

fn program_of(word: &str) -> String {
    if let Some(v) = word.strip_prefix('$') {
        let v = v.trim_start_matches('{');
        let end = v.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(v.len());
        return format!("${}", &v[..end]);
    }
    word.rsplit('/').next().unwrap_or(word).to_string()
}

fn is_net(prog: &str, rest: &[String]) -> bool {
    if let Some(v) = prog.strip_prefix('$') {
        return NET_VARS.contains(&v);
    }
    if prog == "git" {
        let mut it = rest.iter();
        while let Some(w) = it.next() {
            if w == "-C" || w == "-c" {
                it.next();
            } else if !w.starts_with('-') {
                return GIT_NET.contains(&w.as_str());
            }
        }
        return false;
    }
    NET_PROGRAMS.contains(&prog)
}

/// The value of `flags` among `rest` (`--max-time 3`, `--max-time=3`, `-m3`), if any.
fn flag_value(rest: &[String], flags: &[&str]) -> Option<String> {
    let mut it = rest.iter().peekable();
    while let Some(w) = it.next() {
        for f in flags {
            if w == f {
                return Some(it.peek().map(|s| s.to_string()).unwrap_or_default());
            }
            if let Some(v) = w.strip_prefix(&format!("{f}=")) {
                return Some(v.to_string());
            }
            if f.len() == 2 && !f.starts_with("--") && w.len() > 2 && w.starts_with(f) {
                return Some(w[2..].to_string());
            }
        }
    }
    None
}

/// Every call-deadline violation in one shell source.
pub fn scan_shell(src: &[u8]) -> Vec<Hit> {
    let lines: Vec<&[u8]> = src.split(|b| *b == b'\n').collect();
    let marked: Vec<bool> = lines.iter().map(|l| shell_comment_marked(l)).collect();
    let skip_above = |t: &[u8]| trimmed(t).starts_with(b"#");
    let mut out = Vec::new();
    for cmd in shell::parse(src) {
        let (_, argv) = cmd.split_env();
        let words: Vec<String> = argv.iter().map(|w| w.unquoted()).collect();
        let Some(first) = argv.first() else { continue };
        let mut i = 0;
        while i < words.len() {
            match words[i].as_str() {
                "command" | "exec" | "builtin" | "nohup" => {
                    if words.get(i + 1).is_some_and(|w| w.starts_with('-')) {
                        i = words.len();
                    } else {
                        i += 1;
                    }
                }
                "env" | "sudo" | "nice" => {
                    i += 1;
                    while words.get(i).is_some_and(|w| w.starts_with('-') || w.contains('=')) {
                        i += 1;
                    }
                }
                _ => break,
            }
        }
        let Some(head) = words.get(i) else { continue };
        let line = argv.get(i).map_or(first.line, |w| w.line);
        let prog = program_of(head);
        let rest = &words[i + 1..];
        let message = if prog == "timeout" {
            let mut k = 0;
            while k < rest.len() && rest[k].starts_with('-') {
                k += if matches!(rest[k].as_str(), "-k" | "-s") { 2 } else { 1 };
            }
            match rest.get(k).and_then(|w| parse_duration_word(w)) {
                Some(s) if s > CAP_SECS => Some(over(s)),
                _ => None,
            }
        } else if is_net(&prog, rest) {
            match prog.as_str() {
                "curl" | "wget" => match flag_value(rest, &["--max-time", "-m", "--timeout", "-T"]) {
                    None => Some("network call without a timeout — pass --max-time / --timeout".to_string()),
                    Some(v) => parse_duration_word(&v).filter(|s| *s > CAP_SECS).map(over),
                },
                "ssh" | "scp" if rest.iter().any(|w| w.contains("ConnectTimeout=")) => rest
                    .iter()
                    .find_map(|w| w.split("ConnectTimeout=").nth(1))
                    .and_then(parse_duration_word)
                    .filter(|s| *s > CAP_SECS)
                    .map(over),
                _ => Some(format!("{prog} call without a timeout — run it under `timeout {CAP_SECS:.0}`")),
            }
        } else {
            None
        };
        if let Some(m) = message {
            if !is_marked(&lines, &marked, line, skip_above) {
                out.push(hit(line, m));
            }
        }
    }
    out.sort_by_key(|h| h.line);
    out
}

fn shell_comment_marked(line: &[u8]) -> bool {
    let t = trimmed(line);
    if t.starts_with(b"#") {
        return marker_re().is_match(t);
    }
    line.windows(2).position(|w| w[0].is_ascii_whitespace() && w[1] == b'#').is_some_and(|p| marker_re().is_match(&line[p + 1..]))
}

fn parse_allow(text: &str) -> Result<BTreeMap<String, (usize, usize)>, LintError> {
    parse_count_allow(ALLOW_FILE, text)
}

pub(crate) fn parse_count_allow(file: &str, text: &str) -> Result<BTreeMap<String, (usize, usize)>, LintError> {
    let mut out = BTreeMap::new();
    for (i, l) in text.lines().enumerate() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let bad = |reason: &str| LintError::BadAllow { file: file.to_string(), line: i + 1, reason: reason.to_string() };
        let (count, path) = t.split_once(char::is_whitespace).ok_or_else(|| bad("want `<count> <path>`"))?;
        let n: usize = count.parse().map_err(|_| bad("the count is not a number"))?;
        if n == 0 {
            return Err(bad("a count of 0 is a line to delete"));
        }
        if out.insert(path.trim().to_string(), (n, i + 1)).is_some() {
            return Err(bad("path listed twice"));
        }
    }
    Ok(out)
}

fn in_scope(path: &str) -> bool {
    if path.starts_with("target/") || path.starts_with("testkit/") {
        return false;
    }
    path.ends_with(".rs") || (path.starts_with("spira/") && (path.ends_with(".sh") || path.starts_with("spira/hooks/")))
}

/// Every violation in the walk, before the allow list: path and hit, sorted.
pub fn violations(tree: &Tree) -> Result<Vec<(String, Hit)>, LintError> {
    let files = crate::scope(tree, &CallDeadline)?;
    let rs: Vec<(&Entry, Shape)> =
        files.iter().filter(|e| e.path.ends_with(".rs")).filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
    let whole = whole_test_files(tree, &rs);
    let mut out = Vec::new();
    for e in &files {
        let Some(src) = tree.content(e) else { continue };
        let hits = if e.path.ends_with(".rs") {
            if whole.contains(&e.path) {
                continue;
            }
            let s = rs.iter().find(|(x, _)| x.path == e.path).map(|(_, s)| s);
            scan_rust(src, &|at| s.is_some_and(|s| s.regions.iter().any(|(a, b)| *a <= at && at < *b)))
        } else {
            scan_shell(src)
        };
        out.extend(hits.into_iter().map(|h| (e.path.clone(), h)));
    }
    out.sort_by(|a, b| (&a.0, a.1.line).cmp(&(&b.0, b.1.line)));
    Ok(out)
}

/// The allow file's text for the tree as it stands: what `--emit-allow` prints.
pub fn render_allow(tree: &Tree) -> Result<String, LintError> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (p, _) in violations(tree)? {
        *counts.entry(p).or_default() += 1;
    }
    let mut s = String::from(
        "# call-deadline: files with call-deadline violations that predate the rule, as\n\
# `<count> <path>`. Generated: spira-lint --only call-deadline --emit-allow.\n\
#\n\
# SHRINK-ONLY. A count goes down as calls gain a timeout of at most 5 s, or a\n\
# `batch-job: <why>` marker where the call really is a batch job; nothing is raised for a NEW call.\n",
    );
    for (p, n) in counts {
        // literal-ok: matches a file path, not the label
        if p.contains("world-stop") {
            s.push_str("# literal-ok: a file path, not the world-stop literal itself\n");
        }
        s.push_str(&format!("{n} {p}\n"));
    }
    Ok(s)
}

impl Rule for CallDeadline {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        in_scope(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let allow = parse_allow(&tree.read_text(ALLOW_FILE))?;
        let mut by_path: BTreeMap<String, Vec<Hit>> = BTreeMap::new();
        for (p, h) in violations(tree)? {
            by_path.entry(p).or_default().push(h);
        }
        let mut out = Vec::new();
        for (path, hits) in &by_path {
            let allowed = allow.get(path).map_or(0, |a| a.0);
            if hits.len() <= allowed {
                continue;
            }
            let note = if allowed == 0 { String::new() } else { format!(" ({} allowed, {} found)", allowed, hits.len()) };
            for h in hits {
                out.push(Finding { rule: NAME, path: path.clone(), line: Some(h.line), message: format!("{}{note}", h.message) });
            }
        }
        for (path, (n, line)) in &allow {
            let now = by_path.get(path).map_or(0, Vec::len);
            if now < *n {
                let fix = if now == 0 { "remove the line".to_string() } else { format!("lower it to {now}") };
                out.push(Finding {
                    rule: NAME,
                    path: ALLOW_FILE.to_string(),
                    line: Some(*line),
                    message: format!("allows {n} for {path}, which now has {now} — {fix} (the list only shrinks)"),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "a call with no deadline blocks its caller for as long as the far side likes, and a long one \
holds a lock, a lease or a slot while it does. Give the call a timeout of at most 5 s (`timeout 5 bd …`, \
curl --max-time, a socket read timeout, a bounded runner). A real batch job (gate, build, test suite, \
release verify) is marked at the call: `// batch-job: <why>` or `# batch-job: <why>`. Existing violations \
are counted in spira-lint/call-deadline-allow, which only shrinks; regenerate with \
`spira-lint --only call-deadline --emit-allow`."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn rs(src: &str) -> Vec<(usize, String)> {
        scan_rust(src.as_bytes(), &|_| false).into_iter().map(|h| (h.line, h.message)).collect()
    }

    fn sh(src: &str) -> Vec<(usize, String)> {
        scan_shell(src.as_bytes()).into_iter().map(|h| (h.line, h.message)).collect()
    }

    #[test]
    fn a_bare_subprocess_call_is_caught() {
        let got = rs("fn f() {\n    let _ = Command::new(\"bd\").output();\n}\n");
        assert_eq!(got, vec![(2, "subprocess call without a timeout".to_string())]);
    }

    #[test]
    fn a_duration_above_the_cap_on_a_call_is_caught() {
        let got = rs("fn f(c: Command) {\n    run(c, Duration::from_secs(60));\n}\nconst T: Duration = Duration::from_millis(5001); // timeout\n");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].1.contains("60s"), "{got:?}");
        let got = rs("const CALL_TIMEOUT: Duration = Duration::from_secs(60);\n");
        assert_eq!(got.len(), 1, "{got:?}");
    }

    #[test]
    fn bounded_calls_and_non_call_durations_pass() {
        assert!(rs("fn f() {\n    let t = Duration::from_secs(5);\n    let _ = Command::new(\"bd\").output();\n    wait_timeout(t);\n}\n").is_empty());
        assert!(rs("fn f() {\n    thread::sleep(Duration::from_secs(60));\n}\n").is_empty());
        assert!(rs("fn f() {\n    let _ = Command::new(\"timeout\").arg(\"3\").arg(\"bd\").output();\n}\n").is_empty());
        assert!(rs("fn f() -> Command {\n    Command::new(\"bd\")\n}\n").is_empty());
        assert_eq!(rs("fn f() {\n    let _ = Command::new(\"timeout\").arg(\"60\").arg(\"bd\").output();\n}\n").len(), 1);
    }

    #[test]
    fn a_socket_without_a_read_timeout_is_caught() {
        assert_eq!(rs("fn f() {\n    let _s = UnixStream::connect(p);\n}\n").len(), 1);
        assert!(rs("fn f() {\n    let s = UnixStream::connect(p);\n    s.set_read_timeout(t);\n}\n").is_empty());
    }

    #[test]
    fn a_rust_batch_marker_exempts_the_call_only_when_it_is_a_comment() {
        assert!(rs("fn f() {\n    // batch-job: the suite runs for minutes\n    let _ = Command::new(\"cargo\").status();\n}\n").is_empty());
        assert!(rs("// batch-job: whole fn\nfn f() {\n    let _ = Command::new(\"cargo\").status();\n}\n").is_empty());
        assert!(rs("fn f() {\n    let _ = Command::new(\"cargo\").status(); // batch-job: build\n}\n").is_empty());
        assert_eq!(rs("fn f() {\n    let _m = \"// batch-job: nope\";\n    let _ = Command::new(\"cargo\").status();\n}\n").len(), 1);
        assert_eq!(rs("fn f() {\n    // batch-job:\n    let _ = Command::new(\"cargo\").status();\n}\n").len(), 1);
        assert_eq!(rs("fn f() {\n    // batch-job: x\n\n    let _ = Command::new(\"cargo\").status();\n}\n").len(), 1);
    }

    #[test]
    fn test_code_is_exempt() {
        let src = "fn f() {\n    let _ = Command::new(\"bd\").output();\n}\n";
        assert!(scan_rust(src.as_bytes(), &|_| true).is_empty());
    }

    #[test]
    fn shell_network_calls_need_a_short_timeout() {
        assert_eq!(sh("#!/bin/sh\nbd list\n").len(), 1);
        assert_eq!(sh("x=$(\"$SPIRA_BD\" show a)\n").len(), 1);
        assert_eq!(sh("git -C d fetch origin\n").len(), 1);
        assert_eq!(sh("curl -s http://x\n").len(), 1);
        assert_eq!(sh("timeout 60 bd list\n"), vec![(1, over(60.0))]);
        assert_eq!(sh("timeout 2m bd list\n").len(), 1);
        assert_eq!(sh("curl --max-time 30 http://x\n"), vec![(1, over(30.0))]);
    }

    #[test]
    fn shell_bounded_and_unrelated_commands_pass() {
        assert!(sh("timeout 5 bd list\ntimeout -k 1 3 \"$BD\" show a\ntimeout \"$T\" bd list\n").is_empty());
        assert!(sh("curl -m 3 http://x\ncurl --max-time=2 http://x\nssh -o ConnectTimeout=3 h true\n").is_empty());
        assert!(sh("git status\ngit -C d log\ncommand -v bd\necho bd list\n# bd list\ncat <<'E'\nbd list\nE\n").is_empty());
    }

    #[test]
    fn a_shell_batch_marker_exempts_the_call() {
        assert!(sh("# batch-job: gate step\ntimeout 600 bash suite.sh\n").is_empty());
        assert!(sh("timeout 600 bash suite.sh # batch-job: suite\n").is_empty());
        assert_eq!(sh("echo '# batch-job: no'\ntimeout 600 bash suite.sh\n").len(), 1);
        assert_eq!(sh("# batch-job:\ntimeout 600 bash suite.sh\n").len(), 1);
        assert_eq!(sh("# batch-job: x\n\ntimeout 600 bash suite.sh\n").len(), 1);
    }

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        CallDeadline.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    const BARE: &str = "fn f() {\n    let _ = Command::new(\"bd\").output();\n}\n";

    #[test]
    fn a_new_violation_fails_and_a_listed_one_passes() {
        let t = TempDir::new("cd-new");
        t.write("a/src/lib.rs", BARE);
        t.write(ALLOW_FILE, "");
        assert_eq!(run(&t, &["a/src/lib.rs"]).unwrap(), vec!["call-deadline: a/src/lib.rs:2: subprocess call without a timeout"]);
        t.write(ALLOW_FILE, "# h\n1 a/src/lib.rs\n");
        assert_eq!(run(&t, &["a/src/lib.rs"]).unwrap(), Vec::<String>::new());
        t.write(ALLOW_FILE, "1 a/src/lib.rs\n");
        t.write("a/src/lib.rs", &format!("{BARE}{BARE}"));
        let got = run(&t, &["a/src/lib.rs"]).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got[0].ends_with("(1 allowed, 2 found)"), "{got:?}");
    }

    #[test]
    fn the_list_only_shrinks() {
        let t = TempDir::new("cd-shrink");
        t.write("a/src/lib.rs", BARE);
        t.write("b/src/lib.rs", "fn f() {}\n");
        t.write(ALLOW_FILE, "3 a/src/lib.rs\n1 b/src/lib.rs\n");
        assert_eq!(
            run(&t, &["a/src/lib.rs", "b/src/lib.rs"]).unwrap(),
            vec![
                "call-deadline: spira-lint/call-deadline-allow:1: allows 3 for a/src/lib.rs, which now has 1 — lower it to 1 (the list only shrinks)",
                "call-deadline: spira-lint/call-deadline-allow:2: allows 1 for b/src/lib.rs, which now has 0 — remove the line (the list only shrinks)",
            ]
        );
    }

    #[test]
    fn a_marked_fixture_passes_and_a_malformed_allow_line_is_refused() {
        let t = TempDir::new("cd-marked");
        t.write("a/src/lib.rs", "fn f() {\n    // batch-job: build\n    let _ = Command::new(\"cargo\").status();\n}\n");
        t.write(ALLOW_FILE, "");
        assert_eq!(run(&t, &["a/src/lib.rs"]).unwrap(), Vec::<String>::new());
        t.write(ALLOW_FILE, "0 a/src/lib.rs\n");
        assert!(matches!(run(&t, &["a/src/lib.rs"]), Err(LintError::BadAllow { .. })));
    }

    #[test]
    fn render_allow_round_trips() {
        let t = TempDir::new("cd-render");
        t.write("a/src/lib.rs", BARE);
        t.write("spira/x.sh", "bd list\nbd show a\n");
        let tree = Tree::from_paths(t.path(), ["a/src/lib.rs", "spira/x.sh"], std::iter::empty::<&str>());
        let text = render_allow(&tree).unwrap();
        assert!(text.contains("\n1 a/src/lib.rs\n2 spira/x.sh\n"), "{text}");
        t.write(ALLOW_FILE, &text);
        assert_eq!(run(&t, &["a/src/lib.rs", "spira/x.sh"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn nothing_in_scope_is_a_refusal() {
        let t = TempDir::new("cd-empty");
        t.write("a.txt", "x\n");
        assert_eq!(run(&t, &["a.txt"]), Err(LintError::EmptyScope));
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(CallDeadline),
    ]
}
