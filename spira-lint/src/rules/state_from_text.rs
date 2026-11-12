//! `state-from-text` — no decision about an object's state is made from text
//! (law-lifecycle-state-is-not-text, promoted to mechanism by sp-e5bdn6). Contract and known
//! limits: DESIGN.md.
//!
//! Existing violations are counted per file and class in `spira-lint/state-from-text-allow`,
//! which only shrinks: a file over its count fails, and a file under it fails until the count
//! is lowered. Nothing is ever added for new code.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust::{self, Class as Byte};
use crate::rules::call_deadline::{fn_spans, FnSpan};
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct StateFromText;

const NAME: &str = "state-from-text";
const ALLOW_FILE: &str = "spira-lint/state-from-text-allow";

/// What kind of text read a refusal is: also the allow file's second column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// (a) a command's output parsed with a text op where the result decides something.
    Output,
    /// (b) "cannot tell" matched as a substring.
    CannotTell,
    /// (b) a lifecycle or unit state word matched as a substring in a fn that runs a command.
    StateWord,
    /// (c) an identity parsed out of a unit name.
    UnitName,
}

impl Class {
    pub const ALL: [Class; 4] = [Class::Output, Class::CannotTell, Class::StateWord, Class::UnitName];

    pub fn key(self) -> &'static str {
        match self {
            Class::Output => "output",
            Class::CannotTell => "cannot-tell",
            Class::StateWord => "state-word",
            Class::UnitName => "unit-name",
        }
    }

    fn from_key(k: &str) -> Option<Class> {
        Class::ALL.into_iter().find(|c| c.key() == k)
    }

    /// The refusal, naming its exit (law-a-refusal-names-its-exit).
    pub fn message(self, what: &str) -> String {
        match self {
            Class::Output => format!(
                "decides state from command output text ({what}) — read a typed value instead: spira_store::lc_state / LcClient, the command's `--json` deserialised into a struct, or its exit code mapped to an enum"
            ),
            Class::CannotTell => format!(
                "matches \"cannot tell\" as a substring ({what}) — \"cannot tell\" is a typed variant: map the exit code to an enum (LC_UNKNOWN, Holds::Unknown, an Err), never search the message"
            ),
            Class::StateWord => format!(
                "matches a state word as a substring of command output ({what}) — read the state as a typed field: spira_store::lc_state::Row, the command's `--json` deserialised into a struct, or its exit code mapped to an enum"
            ),
            Class::UnitName => format!(
                "derives an identity by parsing a unit name ({what}) — read the identity from a typed record: the lifecycle row (spira_store::lc_state) or the unit's own typed properties; spira_config::unit builds names, nothing parses them"
            ),
        }
    }
}

/// One refusal in one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub line: usize,
    pub class: Class,
    pub message: String,
}

/// The commands whose output is never read as state.
const SOURCES: &[&str] = &["spira-lc", "bd", "bdq", "spira-bd", "systemctl", "queue", "gh", "work"];

/// The words a lifecycle row, a gate, a PR or a unit reports as its state.
const STATE_WORDS: &str = r"\b(?:READY|WORKING|SUBMITTED|REWORK|LANDED|DROPPED|CERTIFIED|CERTIFYING|OPEN|CLOSED|BLOCKED|DEFERRED|HELD|MERGED|PASS|FAIL|RUNNING|active|inactive|failed|activating|deactivating)\b";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

const KEYWORDS: &[&[u8]] = &[
    b"mut", b"ref", b"let", b"in", b"if", b"else", b"true", b"false", b"self", b"_", b"box", b"move", b"match", b"for", b"while", b"return", b"as",
];

/// Byte ranges of each string or char literal.
fn literal_ranges(class: &[Byte]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < class.len() {
        if class[i] == Byte::Literal {
            let s = i;
            while i < class.len() && class[i] == Byte::Literal {
                i += 1;
            }
            out.push((s, i));
        } else {
            i += 1;
        }
    }
    out
}

/// A literal's contents, without its quotes, prefix and hashes.
fn content(src: &[u8], (a, b): (usize, usize)) -> &[u8] {
    let mut s = &src[a..b];
    if let [b'b', rest @ ..] = s {
        s = rest;
    }
    if let [b'r', rest @ ..] = s {
        s = rest;
    }
    while let [b'#', rest @ ..] = s {
        s = rest;
    }
    if let [b'"' | b'\'', rest @ ..] = s {
        s = rest;
    }
    while let [rest @ .., b'#'] = s {
        s = rest;
    }
    if let [rest @ .., b'"' | b'\''] = s {
        s = rest;
    }
    s
}

/// The code segment around `at`: between the nearest `;`, `{` or `}` on each side, held to `[lo, hi)`.
fn segment(code: &[u8], at: usize, lo: usize, hi: usize) -> (usize, usize) {
    let from = code[lo..at].iter().rposition(|b| matches!(b, b';' | b'{' | b'}')).map_or(lo, |p| lo + p + 1);
    let to = code[at..hi].iter().position(|b| matches!(b, b';' | b'{' | b'}')).map_or(hi, |p| at + p);
    (from, to)
}

/// From `at`, the offset of the first `stop` byte at bracket depth 0, held to `hi`.
fn at_depth0(code: &[u8], at: usize, hi: usize, stop: &[u8]) -> usize {
    let mut depth = 0i32;
    for (i, &b) in code.iter().enumerate().take(hi).skip(at) {
        if depth == 0 && stop.contains(&b) {
            return i;
        }
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return i;
        }
    }
    hi
}

/// The binding names in a pattern: lowercase identifiers that are not paths, calls or keywords.
fn pattern_names(pat: &[u8]) -> Vec<Vec<u8>> {
    static ID: OnceLock<Regex> = OnceLock::new();
    let mut out = Vec::new();
    for m in re(&ID, r"\b[a-z_][a-z0-9_]*\b").find_iter(pat) {
        let before = pat[..m.start()].iter().rev().find(|b| !b.is_ascii_whitespace());
        let after = pat[m.end()..].iter().find(|b| !b.is_ascii_whitespace());
        if matches!(before, Some(b'.') | Some(b':')) || matches!(after, Some(b'(') | Some(b':') | Some(b'!')) {
            continue;
        }
        if KEYWORDS.contains(&m.as_bytes()) {
            continue;
        }
        out.push(m.as_bytes().to_vec());
    }
    out
}

/// A binding read from text: its name, and the byte range it is in scope for.
type Taint = BTreeSet<(Vec<u8>, usize, usize)>;

fn bind(taint: &mut Taint, names: Vec<Vec<u8>>, from: usize, to: usize) {
    taint.extend(names.into_iter().map(|n| (n, from, to)));
}

/// Whether `range` of `code` names a tainted binding in its scope (not as a field of something else).
fn mentions(code: &[u8], (a, b): (usize, usize), taint: &Taint) -> bool {
    static ID: OnceLock<Regex> = OnceLock::new();
    if taint.is_empty() {
        return false;
    }
    re(&ID, r"\b[a-z_][a-z0-9_]*\b").find_iter(&code[a..b]).any(|m| {
        let before = code[..a + m.start()].iter().rev().find(|c| !c.is_ascii_whitespace());
        let at = a + m.start();
        before != Some(&b'.') && taint.iter().any(|(n, f, t)| n.as_slice() == m.as_bytes() && *f <= at && at < *t)
    })
}

/// A typed read: a range holding one of these is the exit, not a text read.
fn sanitized(code: &[u8]) -> bool {
    static S: OnceLock<Regex> = OnceLock::new();
    re(&S, r"serde_json\s*::|\bfrom_str\b|\bfrom_slice\b|\bfrom_reader\b|\blc_state\s*::|\bparse_rows\b|\bparse_show\b|\bparse_\w*row\w*\b|\bfirst_row\b|\btoml\s*::|\.\s*status\b|\.\s*code\b|\.\s*success\s*\(|\bexit_code\b")
        .is_match(code)
}

/// The first literal that starts the argument list opening at `open` (just past the `(`).
fn literal_arg(src: &[u8], code: &[u8], lits: &[(usize, usize)], open: usize) -> Option<(usize, usize)> {
    let mut i = open;
    while i < code.len() && (code[i].is_ascii_whitespace() || code[i] == b'&') && !lits.iter().any(|l| l.0 == i) {
        i += 1;
    }
    let _ = src;
    lits.iter().copied().find(|l| l.0 == i)
}

struct Scan<'a> {
    src: &'a [u8],
    code: Vec<u8>,
    lits: Vec<(usize, usize)>,
}

impl Scan<'_> {
    /// Literal positions in `span` that name a source command being run.
    fn source_literals(&self, lo: usize, hi: usize) -> Vec<usize> {
        static CALLISH: OnceLock<Regex> = OnceLock::new();
        static PATHISH: OnceLock<Regex> = OnceLock::new();
        let callish = re(&CALLISH, r"Command\s*::\s*new|\.\s*args?\s*\(|\bexec\w*\s*\(|\brun\w*\s*\(|\bcall\w*\s*\(|\.\s*output\s*\(|\bspawn\w*\s*\(|\bsh\s*\(|\bcmd\w*\s*\(");
        let pathish = re(&PATHISH, r"(?:join|from|Path\s*::\s*new|PathBuf\s*::\s*from|var|var_os|get|insert|push_str|with_extension)\s*\(\s*&?\s*$");
        let mut out = Vec::new();
        for &(a, b) in self.lits.iter().filter(|l| l.0 >= lo && l.1 <= hi) {
            let c = content(self.src, (a, b));
            let named = SOURCES.iter().any(|s| c == s.as_bytes() || (c.ends_with(s.as_bytes()) && c.len() > s.len() && c[c.len() - s.len() - 1] == b'/'));
            if !named {
                continue;
            }
            let back = &self.code[a.saturating_sub(24).max(lo)..a];
            if pathish.is_match(back) {
                continue;
            }
            let (f, t) = segment(&self.code, a, lo, hi);
            if callish.is_match(&self.code[f..t]) {
                out.push(a);
            }
        }
        // `Command::new(&self.bdq)`, `Command::new(systemctl)`, and a port's `.bd(…)` / `.spira_lc(…)`.
        static BIN_VAR: OnceLock<Regex> = OnceLock::new();
        let bin_var = re(
            &BIN_VAR,
            r"Command\s*::\s*new\s*\(\s*&?\s*(?:[a-z_]+\s*\.\s*)*(?:bd|bdq|real_bd|bd_bin|bdq_bin|spira_bd|spira_lc|lc|lc_bin|claim_bin|systemctl|systemctl_bin|gh|gh_bin|queue|queue_bin|work|work_bin)\s*(?:\(\s*\))?\s*\)|\.\s*(?:bd|bdq|spira_lc|lc|systemctl|gh)\s*\(",
        );
        out.extend(bin_var.find_iter(&self.code[lo..hi]).map(|m| lo + m.start()));
        out
    }
}

/// Every refusal in one Rust source; `in_test` says whether a byte offset is test code.
pub fn scan(src: &[u8], in_test: &dyn Fn(usize) -> bool) -> Vec<Hit> {
    static PRED: OnceLock<Regex> = OnceLock::new();
    static SPLIT: OnceLock<Regex> = OnceLock::new();
    static EQ: OnceLock<Regex> = OnceLock::new();
    static REGEX_NEW: OnceLock<Regex> = OnceLock::new();
    static TEXT_CALL: OnceLock<Regex> = OnceLock::new();
    static STATE: OnceLock<Regex> = OnceLock::new();
    static UNIT_LIT: OnceLock<Regex> = OnceLock::new();
    static UNIT_RE: OnceLock<Regex> = OnceLock::new();
    static ROLE_LIT: OnceLock<Regex> = OnceLock::new();
    static FN_NAME: OnceLock<Regex> = OnceLock::new();
    static RET_STRING: OnceLock<Regex> = OnceLock::new();
    static LET: OnceLock<Regex> = OnceLock::new();
    static FOR: OnceLock<Regex> = OnceLock::new();
    static IFLET: OnceLock<Regex> = OnceLock::new();
    static MATCH: OnceLock<Regex> = OnceLock::new();
    static CLOSURE: OnceLock<Regex> = OnceLock::new();
    static HEADER: OnceLock<Regex> = OnceLock::new();
    let cl = rust::classify(src);
    let s = Scan { src, code: cl.code_only(), lits: literal_ranges(&cl.class) };
    let code = &s.code;
    let mut hits: BTreeMap<usize, (Class, String)> = BTreeMap::new();
    let rank = |c: Class| match c {
        Class::CannotTell => 0,
        Class::StateWord => 1,
        Class::UnitName => 2,
        Class::Output => 3,
    };
    let mut put = |at: usize, class: Class, what: String| {
        let line = cl.line_of(at);
        match hits.get(&line) {
            Some((c, _)) if rank(*c) <= rank(class) => {}
            _ => {
                hits.insert(line, (class, what));
            }
        }
    };

    // (b) "cannot tell" and (c) unit-name parsing: anywhere in non-test code.
    let text_call = re(
        &TEXT_CALL,
        r"\.\s*(contains|starts_with|ends_with|strip_prefix|strip_suffix|trim_start_matches|trim_end_matches|split_once|rsplit_once|split|rsplit|splitn|find|rfind|matches|replace|is_match|captures|eq|eq_ignore_ascii_case)\s*\(",
    );
    // Extracting a part of a unit name, or testing one for a role prefix (`spira-dwarf-`).
    // Testing only the kind (`.ends_with(".service")`) or the family (`starts_with("spira-")`)
    // classifies a unit file and is not an identity.
    let unit_lit = re(&UNIT_LIT, r"^(?:spira-[a-z0-9_]+-[a-z0-9_-]*|spira-|[A-Za-z0-9_@.-]*\.(?:service|timer|scope|socket|slice))$");
    let role_lit = re(&ROLE_LIT, r"^spira-[a-z0-9_]+-[a-z0-9_-]*$");
    let extract_ops = ["strip_prefix", "strip_suffix", "trim_start_matches", "trim_end_matches", "split_once", "rsplit_once", "split", "rsplit", "splitn", "replace"];
    let test_ops = ["starts_with", "ends_with", "find", "rfind", "contains"];
    let regex_new = re(&REGEX_NEW, r"\bRegex\s*::\s*new\s*\(|\bRegexBuilder\s*::\s*new\s*\(");
    let unit_re = re(&UNIT_RE, r"\\\.(?:service|timer|scope)\b|spira-[a-z0-9_]+-\(");
    let cannot = |c: &[u8]| c.to_ascii_lowercase().windows(11).any(|w| w == b"cannot tell");
    for c in text_call.captures_iter(code) {
        let m = c.get(0).unwrap();
        if in_test(m.start()) {
            continue;
        }
        let op = String::from_utf8_lossy(&c[1]).into_owned();
        let Some(l) = literal_arg(src, code, &s.lits, m.end()) else { continue };
        let lit = content(src, l);
        if cannot(lit) {
            put(m.start(), Class::CannotTell, format!(".{op}(\"{}\")", String::from_utf8_lossy(lit)));
        } else if (extract_ops.contains(&op.as_str()) && unit_lit.is_match(lit)) || (test_ops.contains(&op.as_str()) && role_lit.is_match(lit)) {
            put(m.start(), Class::UnitName, format!(".{op}(\"{}\")", String::from_utf8_lossy(lit)));
        }
    }
    for m in regex_new.find_iter(code) {
        if in_test(m.start()) {
            continue;
        }
        let Some(l) = literal_arg(src, code, &s.lits, m.end()) else { continue };
        let lit = content(src, l);
        if cannot(lit) {
            put(m.start(), Class::CannotTell, "a Regex over \"cannot tell\"".into());
        } else if unit_re.is_match(lit) {
            put(m.start(), Class::UnitName, format!("Regex::new(\"{}\")", String::from_utf8_lossy(lit)));
        }
    }
    let eq = re(&EQ, r"[=!]=");
    for m in eq.find_iter(code) {
        if in_test(m.start()) {
            continue;
        }
        let side = s.lits.iter().find(|l| {
            let gap_after = l.0 >= m.end() && code[m.end()..l.0].iter().all(|b| b.is_ascii_whitespace() || *b == b'&' || *b == b'*');
            let gap_before = l.1 <= m.start() && code[l.1..m.start()].iter().all(|b| b.is_ascii_whitespace());
            gap_after || gap_before
        });
        if let Some(l) = side {
            if cannot(content(src, *l)) {
                put(m.start(), Class::CannotTell, "== against \"cannot tell\"".into());
            }
        }
    }

    // (a) command output read as state, and (b) state words, per fn.
    let spans: Vec<FnSpan> = fn_spans(code);
    let fn_name = re(&FN_NAME, r"\bfn\s+([A-Za-z_]\w*)");
    let ret_string = re(&RET_STRING, r"->\s*(?:(?:Option|Result|Vec)\s*<\s*)*(?:String\b|(?:std\s*::\s*process\s*::\s*)?Command\b|\(\s*i32\s*,\s*String\s*\)|&\s*(?:'\w+\s+)?str\b)");
    let sig_of = |sp: &FnSpan| -> (Vec<u8>, bool) {
        let open = code[sp.start..sp.end].iter().position(|b| *b == b'{').map_or(sp.end, |p| sp.start + p);
        let sig = &code[sp.start..open];
        let name = fn_name.captures(sig).map(|c| c[1].to_vec()).unwrap_or_default();
        (name, ret_string.is_match(sig))
    };
    // Same-file helpers that run a source command and hand back its text.
    let mut runners: BTreeSet<Vec<u8>> = BTreeSet::new();
    // Where the file calls one of `runners` (not where it defines one).
    let calls_of = |runners: &BTreeSet<Vec<u8>>| -> Vec<usize> {
        if runners.is_empty() {
            return Vec::new();
        }
        let alt: Vec<String> = runners.iter().map(|n| regex::escape(&String::from_utf8_lossy(n))).collect();
        let r = Regex::new(&format!(r"\b(?:{})\s*\(", alt.join("|"))).expect("escaped names");
        r.find_iter(code).map(|m| m.start()).filter(|&p| !code[p.saturating_sub(8)..p].ends_with(b"fn ")).collect()
    };
    let sources_in = |sp: &FnSpan, calls: &[usize]| -> Vec<usize> {
        let mut v = s.source_literals(sp.start, sp.end);
        v.extend(calls.iter().copied().filter(|p| sp.start <= *p && *p < sp.end));
        v
    };
    let mut calls: Vec<usize> = Vec::new();
    for _ in 0..3 {
        let before = runners.len();
        for sp in &spans {
            let (name, ret) = sig_of(sp);
            if ret && !name.is_empty() && !sources_in(sp, &calls).is_empty() {
                runners.insert(name);
            }
        }
        if runners.len() == before {
            break;
        }
        calls = calls_of(&runners);
    }

    let pred = re(
        &PRED,
        r"\.\s*(contains|starts_with|ends_with|strip_prefix|strip_suffix|split_once|rsplit_once|is_match|captures|captures_iter|find_iter|eq_ignore_ascii_case)\s*\(|\bmatches\s*!\s*\(",
    );
    let split = re(&SPLIT, r"\.\s*(lines|split|rsplit|splitn|split_whitespace|split_terminator|find|rfind|len|count|chars|bytes|trim|trim_start|trim_end|trim_start_matches|trim_end_matches|next|nth|last|first)\s*\(");
    let state = re(&STATE, STATE_WORDS);
    let let_re = re(&LET, r"\blet\b");
    let for_re = re(&FOR, r"\bfor\b");
    let iflet = re(&IFLET, r"\b(?:if|while)\s+let\b");
    let match_re = re(&MATCH, r"\bmatch\b");
    let closure = re(&CLOSURE, r"\|([^|{}();]*)\|");
    let header = re(&HEADER, r"^\s*(?:else\s+)?(?:if|while|match)\b|\belse\s*$");
    for sp in &spans {
        if in_test(sp.start) {
            continue;
        }
        // A nested fn is judged as its own span.
        let inner: Vec<(usize, usize)> = spans.iter().filter(|o| o.start > sp.start && o.end <= sp.end).map(|o| (o.start, o.end)).collect();
        let own = |p: usize| !inner.iter().any(|(a, b)| *a <= p && p < *b);
        let seeds: Vec<usize> = sources_in(sp, &calls).into_iter().filter(|p| own(*p)).collect();
        if seeds.is_empty() {
            continue;
        }
        let (lo, hi) = (sp.start, sp.end);
        let mut taint = Taint::new();
        let hot = |r: (usize, usize), taint: &Taint| seeds.iter().any(|p| r.0 <= *p && *p < r.1) || mentions(code, r, taint);
        for _ in 0..8 {
            let n = taint.len();
            for m in let_re.find_iter(&code[lo..hi]) {
                let at = lo + m.end();
                let end = at_depth0(code, at, hi, b";");
                let Some(eq) = (at..end).find(|&i| code[i] == b'=' && !matches!(code.get(i + 1), Some(b'=' | b'>')) && !matches!(code[i - 1], b'=' | b'!' | b'<' | b'>')) else { continue };
                if eq >= end {
                    continue;
                }
                let pat_end = (at..eq).find(|&i| code[i] == b':' && code.get(i + 1) != Some(&b':') && code[i - 1] != b':').unwrap_or(eq);
                let rhs = (eq + 1, end);
                if hot(rhs, &taint) && !sanitized(&code[rhs.0..rhs.1]) {
                    bind(&mut taint, pattern_names(&code[at..pat_end]), end, hi);
                }
            }
            for m in for_re.find_iter(&code[lo..hi]) {
                let at = lo + m.end();
                let open = at_depth0(code, at, hi, b"{;");
                let hdr = &code[at..open];
                let Some(inp) = hdr.windows(4).position(|w| w == b" in ") else { continue };
                let expr = (at + inp + 4, open);
                if hot(expr, &taint) && !sanitized(&code[expr.0..expr.1]) {
                    let close = if open < hi && code[open] == b'{' { at_depth0(code, open + 1, hi, b"") } else { open };
                    bind(&mut taint, pattern_names(&hdr[..inp]), open, close);
                }
            }
            for m in iflet.find_iter(&code[lo..hi]) {
                let at = lo + m.end();
                let open = at_depth0(code, at, hi, b"{;");
                let Some(eq) = (at..open).find(|&i| code[i] == b'=' && !matches!(code.get(i + 1), Some(b'=' | b'>')) && !matches!(code[i - 1], b'=' | b'!' | b'<' | b'>')) else { continue };
                if hot((eq + 1, open), &taint) && !sanitized(&code[eq + 1..open]) {
                    let close = if open < hi && code[open] == b'{' { at_depth0(code, open + 1, hi, b"") } else { open };
                    bind(&mut taint, pattern_names(&code[at..eq]), open, close);
                }
            }
            for m in match_re.find_iter(&code[lo..hi]) {
                let at = lo + m.end();
                let open = at_depth0(code, at, hi, b"{;");
                if open >= hi || code[open] != b'{' || !hot((at, open), &taint) || sanitized(&code[at..open]) {
                    continue;
                }
                let close = at_depth0(code, open + 1, hi, b"");
                // Arms: from each depth-1 `{`/`,`/`}` to the next `=>`.
                let mut depth = 0i32;
                let mut arm_start = open + 1;
                let mut i = open + 1;
                while i < close {
                    match code[i] {
                        b'(' | b'[' | b'{' => depth += 1,
                        b')' | b']' | b'}' => {
                            depth -= 1;
                            if depth == 0 && code[i] == b'}' {
                                arm_start = i + 1;
                            }
                        }
                        b',' if depth == 0 => arm_start = i + 1,
                        b'=' if depth == 0 && code.get(i + 1) == Some(&b'>') => {
                            let pat = &code[arm_start..i];
                            let pat = pat.windows(4).position(|w| w == b" if ").map_or(pat, |g| &pat[..g]);
                            bind(&mut taint, pattern_names(pat), open, close);
                            arm_start = i + 2;
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
            for c in closure.captures_iter(&code[lo..hi]) {
                let m = c.get(0).unwrap();
                let at = lo + m.start();
                let prev = code[lo..at].iter().rev().find(|b| !b.is_ascii_whitespace());
                if !matches!(prev, Some(b'(' | b',' | b'=')) && !code[lo..at].trim_ascii_end().ends_with(b"move") {
                    continue;
                }
                if hot(segment(code, at, lo, hi), &taint) {
                    let body = lo + m.end();
                    bind(&mut taint, pattern_names(&c[1]), body, at_depth0(code, body, hi, b",)"));
                }
            }
            if taint.len() == n {
                break;
            }
        }
        let hot_at = |at: usize| hot(segment(code, at, lo, hi), &taint);
        let sanitized_at = |at: usize| {
            let (f, t) = segment(code, at, lo, hi);
            sanitized(&code[f..t])
        };
        for c in pred.captures_iter(&code[lo..hi]) {
            let m = c.get(0).unwrap();
            let at = lo + m.start();
            if !own(at) {
                continue;
            }
            let op = c.get(1).map_or("matches!".to_string(), |o| format!(".{}()", String::from_utf8_lossy(o.as_bytes())));
            if c.get(1).is_none() {
                let close = at_depth0(code, lo + m.end(), hi, b")");
                if !s.lits.iter().any(|l| l.0 >= lo + m.end() && l.1 <= close && code.get(l.0) != Some(&b'\'')) {
                    continue;
                }
            }
            if hot_at(at) && !sanitized_at(at) {
                put(at, Class::Output, op);
            }
        }
        for c in text_call.captures_iter(&code[lo..hi]) {
            let m = c.get(0).unwrap();
            let at = lo + m.start();
            if !own(at) {
                continue;
            }
            if let Some(l) = literal_arg(src, code, &s.lits, lo + m.end()) {
                if state.is_match(content(src, l)) {
                    put(at, Class::StateWord, format!(".{}(\"{}\")", String::from_utf8_lossy(&c[1]), String::from_utf8_lossy(content(src, l))));
                }
            }
        }
        for m in regex_new.find_iter(&code[lo..hi]) {
            let at = lo + m.start();
            if let Some(l) = literal_arg(src, code, &s.lits, lo + m.end()) {
                if own(at) && state.is_match(content(src, l)) {
                    put(at, Class::StateWord, "a Regex over a state word".into());
                }
            }
        }
        for m in eq.find_iter(&code[lo..hi]) {
            let at = lo + m.start();
            if !own(at) {
                continue;
            }
            let lit_side = s.lits.iter().any(|l| {
                (l.0 >= at + 2 && l.0 < hi && code[at + 2..l.0].iter().all(|b| b.is_ascii_whitespace() || *b == b'&' || *b == b'*'))
                    || (l.1 <= at && l.0 >= lo && code[l.1..at].iter().all(|b| b.is_ascii_whitespace()))
            });
            if lit_side && hot_at(at) && !sanitized_at(at) {
                put(at, Class::Output, format!("{} against a literal", String::from_utf8_lossy(&code[at..at + 2])));
            }
        }
        for c in split.captures_iter(&code[lo..hi]) {
            let m = c.get(0).unwrap();
            let at = lo + m.start();
            if !own(at) {
                continue;
            }
            let (f, t) = segment(code, at, lo, hi);
            let is_header = t < hi && code[t] == b'{' && header.is_match(&code[f..t]);
            if is_header && hot((f, t), &taint) && !sanitized(&code[f..t]) {
                put(at, Class::Output, format!(".{}() in a branch condition", String::from_utf8_lossy(&c[1])));
            }
        }
        // A match on output text with string-literal arms.
        for m in match_re.find_iter(&code[lo..hi]) {
            let at = lo + m.start();
            let open = at_depth0(code, at + 5, hi, b"{;");
            if !own(at) || open >= hi || code[open] != b'{' || !hot((at, open), &taint) || sanitized(&code[at..open]) {
                continue;
            }
            let close = at_depth0(code, open + 1, hi, b"");
            let literal_arm = s.lits.iter().filter(|l| l.0 > open && l.1 < close).any(|l| {
                let next = code[l.1..close].iter().position(|b| !b.is_ascii_whitespace()).map(|p| l.1 + p);
                next.is_some_and(|n| (code[n] == b'=' && code.get(n + 1) == Some(&b'>')) || (code[n] == b'|' && code.get(n + 1) != Some(&b'|')))
            });
            if literal_arm {
                put(at, Class::Output, "a match with string arms".into());
            }
        }
    }
    hits.into_iter().map(|(line, (class, what))| Hit { line, class, message: class.message(&what) }).collect()
}

fn in_scope(path: &str) -> bool {
    path.ends_with(".rs")
        && !path.starts_with("target/")
        && !path.starts_with("testkit/")
        && !path.starts_with("spira-lint/")
        && !path.contains("/tests/")
        && !path.starts_with("tests/")
}

/// Every violation in the walk, before the allow list: path and hit, sorted.
pub fn violations(tree: &Tree) -> Result<Vec<(String, Hit)>, LintError> {
    let files = crate::scope(tree, &StateFromText)?;
    let shapes: Vec<(&Entry, Shape)> = files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
    let whole = whole_test_files(tree, &shapes);
    let mut out = Vec::new();
    for (e, s) in &shapes {
        let Some(src) = tree.content(e) else { continue };
        if s.inner_test || whole.contains(&e.path) || crate::rust_test::test_by_path(&e.path) {
            continue;
        }
        for h in scan(src, &|at| s.covers(at)) {
            out.push((e.path.clone(), h));
        }
    }
    out.sort_by(|a, b| (&a.0, a.1.line).cmp(&(&b.0, b.1.line)));
    Ok(out)
}

/// `(path, class)` -> (allowed count, allow-file line).
type Allow = BTreeMap<(String, Class), (usize, usize)>;

/// The allow file: `<count> <class> <path>` per line.
pub fn parse_allow(text: &str) -> Result<Allow, LintError> {
    let mut out = Allow::new();
    for (i, l) in text.lines().enumerate() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let bad = |reason: &str| LintError::BadAllow { file: ALLOW_FILE.to_string(), line: i + 1, reason: reason.to_string() };
        let mut w = t.split_whitespace();
        let (Some(count), Some(class), Some(path), None) = (w.next(), w.next(), w.next(), w.next()) else {
            return Err(bad("want `<count> <class> <path>`"));
        };
        let n: usize = count.parse().map_err(|_| bad("the count is not a number"))?;
        if n == 0 {
            return Err(bad("a count of 0 is a line to delete"));
        }
        let class = Class::from_key(class).ok_or_else(|| bad("the class is not one of output, cannot-tell, state-word, unit-name"))?;
        if out.insert((path.to_string(), class), (n, i + 1)).is_some() {
            return Err(bad("path and class listed twice"));
        }
    }
    Ok(out)
}

const HEADER: &str = "\
# state-from-text: files that decide an object's state from text and predate the rule
# (sp-e5bdn6, law-lifecycle-state-is-not-text), as `<count> <class> <path>`.
# Classes: output (command output parsed into a decision), cannot-tell (\"cannot tell\" as a
# substring), state-word (a state word as a substring), unit-name (identity parsed from a
# unit name). Generated: spira-lint --only state-from-text --emit-allow.
#
# SHRINK-ONLY. Never add a line for new code. A count goes down as a site moves to a typed
# read (spira_store::lc_state / LcClient, `--json` into a struct, an exit-code enum); a file
# under its count fails until the line is lowered or removed.
";

/// The allow file's text for the tree as it stands: what `--emit-allow` prints.
pub fn render_allow(tree: &Tree) -> Result<String, LintError> {
    let mut counts: BTreeMap<(String, Class), usize> = BTreeMap::new();
    for (p, h) in violations(tree)? {
        *counts.entry((p, h.class)).or_default() += 1;
    }
    let mut s = String::from(HEADER);
    for ((p, c), n) in counts {
        s.push_str(&format!("{n} {} {p}\n", c.key()));
    }
    Ok(s)
}

impl Rule for StateFromText {
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
        let mut found: BTreeMap<(String, Class), Vec<Hit>> = BTreeMap::new();
        for (p, h) in violations(tree)? {
            found.entry((p, h.class)).or_default().push(h);
        }
        let mut out = Vec::new();
        for ((path, class), hits) in &found {
            let allowed = allow.get(&(path.clone(), *class)).map_or(0, |a| a.0);
            if hits.len() <= allowed {
                continue;
            }
            let note = if allowed == 0 { String::new() } else { format!(" ({allowed} {} allowed, {} found)", class.key(), hits.len()) };
            for h in hits {
                out.push(Finding { rule: NAME, path: path.clone(), line: Some(h.line), message: format!("{}{note}", h.message) });
            }
        }
        for ((path, class), (n, line)) in &allow {
            let now = found.get(&(path.clone(), *class)).map_or(0, Vec::len);
            if now < *n {
                let fix = if now == 0 { "remove the line".to_string() } else { format!("lower it to {now}") };
                out.push(Finding {
                    rule: NAME,
                    path: ALLOW_FILE.to_string(),
                    line: Some(*line),
                    message: format!("allows {n} {} for {path}, which now has {now} — {fix} (the list only shrinks)", class.key()),
                });
            }
        }
        out.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "a decision about an object's state (bead, round, batch, ask, unit, release, mail) reads a \
typed value, never text (law-lifecycle-state-is-not-text): an in-process typed API \
(spira_store::lc_state, LcClient), the command's `--json` answer deserialised into a struct, or \
its exit code mapped to an enum. \"Cannot tell\" is a typed variant, never a substring, and a \
unit's identity comes from a typed record, never from parsing its name. Existing sites are \
counted in spira-lint/state-from-text-allow, which only shrinks and never takes a line for new \
code; regenerate with `spira-lint --only state-from-text --emit-allow`."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(StateFromText)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn classes(src: &str) -> Vec<(usize, Class)> {
        scan(src.as_bytes(), &|_| false).into_iter().map(|h| (h.line, h.class)).collect()
    }

    fn refused(src: &str, class: Class) {
        let got = classes(src);
        assert!(got.iter().any(|(_, c)| *c == class), "want a {class:?} refusal, got {got:?} for:\n{src}");
    }

    fn passes(src: &str) {
        let got = classes(src);
        assert!(got.is_empty(), "want no refusal, got {got:?} for:\n{src}");
    }

    // ---- (a) process output read as state -------------------------------------------------

    /// The production scar (sp-druc64): holds read from `spira-lc holds` stdout, line by line.
    #[test]
    fn holds_read_from_lc_lines_are_refused() {
        refused(
            "pub fn read_holds(exec: &dyn Exec, id: &str) -> Holds {\n    match ask(|| exec.exec(\"spira-lc\", &s(&[\"holds\", id]), None, None)) {\n        Ok(text) => Holds::Known(text.lines().map(str::trim).filter(|h| !h.is_empty() && *h != \"wait\").map(String::from).collect()),\n        Err(why) => Holds::Unknown(why),\n    }\n}\n",
            Class::Output,
        );
    }

    #[test]
    fn stdout_contains_in_a_branch_is_refused() {
        refused(
            "fn active(u: &str) -> bool {\n    let out = Command::new(\"timeout\").args([\"5\", \"systemctl\", \"is-active\", u]).output().unwrap();\n    let text = String::from_utf8_lossy(&out.stdout);\n    if text.contains(\"could not be found\") {\n        return false;\n    }\n    true\n}\n",
            Class::Output,
        );
    }

    #[test]
    fn starts_with_and_ends_with_on_output_are_refused() {
        refused(
            "fn f() {\n    let o = Command::new(\"gh\").args([\"pr\", \"view\"]).output().unwrap();\n    let s = String::from_utf8(o.stdout).unwrap();\n    for l in s.lines() {\n        if l.starts_with(\"state:\") {\n            go();\n        }\n    }\n}\n",
            Class::Output,
        );
        refused(
            "fn f() {\n    let o = Command::new(\"queue\").arg(\"show\").output().unwrap();\n    let s = String::from_utf8_lossy(&o.stdout).to_string();\n    let done = s.trim().ends_with(\"done\");\n    if done { go(); }\n}\n",
            Class::Output,
        );
    }

    #[test]
    fn equality_against_a_word_from_output_is_refused() {
        refused(
            "fn f(id: &str) {\n    let state = run(\"bdq\", &[\"state\", id]);\n    if state.trim() == \"closed\" {\n        go();\n    }\n}\n",
            Class::Output,
        );
    }

    #[test]
    fn a_match_on_output_text_is_refused() {
        refused(
            "fn f(id: &str) -> u8 {\n    let o = Command::new(\"spira-lc\").args([\"state\", id]).output().unwrap();\n    let s = String::from_utf8_lossy(&o.stdout);\n    match s.trim() {\n        \"x\" => 1,\n        _ => 0,\n    }\n}\n",
            Class::Output,
        );
    }

    #[test]
    fn a_regex_over_output_is_refused() {
        refused(
            "fn f() -> bool {\n    let o = Command::new(\"work\").args([\"read\", \"x\"]).output().unwrap();\n    let s = String::from_utf8_lossy(&o.stdout);\n    let re = Regex::new(r\"\\[P0 · (\\w+)\\]\").unwrap();\n    if let Some(c) = re.captures(&s) {\n        return &c[1] == \"y\";\n    }\n    false\n}\n",
            Class::Output,
        );
    }

    #[test]
    fn split_of_output_in_a_branch_header_is_refused() {
        refused(
            "fn f() {\n    let o = Command::new(\"bd\").args([\"list\"]).output().unwrap();\n    let s = String::from_utf8_lossy(&o.stdout);\n    if s.lines().count() > 1 {\n        go();\n    }\n}\n",
            Class::Output,
        );
    }

    /// A same-file helper that returns a command's text taints its callers.
    #[test]
    fn a_text_runner_helper_taints_its_callers() {
        refused(
            "fn lc_text(args: &[&str]) -> String {\n    let o = Command::new(\"spira-lc\").args(args).output().unwrap();\n    String::from_utf8_lossy(&o.stdout).into_owned()\n}\nfn held(id: &str) -> bool {\n    let t = lc_text(&[\"holds\", id]);\n    t.lines().any(|l| l == \"ryan\")\n}\n",
            Class::Output,
        );
    }

    // ---- (b) "cannot tell" and state words as substrings ----------------------------------

    #[test]
    fn cannot_tell_as_a_substring_is_refused_anywhere() {
        refused("fn f(e: &str) -> bool {\n    e.contains(\"cannot tell\")\n}\n", Class::CannotTell);
        refused("fn f(e: &str) -> bool {\n    e.starts_with(\"spira-lc: cannot tell: deadline\")\n}\n", Class::CannotTell);
        refused("fn f(e: &str) -> bool {\n    Regex::new(\"(?i)cannot tell\").unwrap().is_match(e)\n}\n", Class::CannotTell);
    }

    #[test]
    fn a_state_word_substring_in_a_command_fn_is_refused() {
        refused(
            "fn f(id: &str) -> bool {\n    let o = Command::new(\"spira-lc\").args([\"show\", id]).output().unwrap();\n    let s = String::from_utf8_lossy(&o.stdout).to_string();\n    s.contains(\"\\\"state\\\": \\\"READY\\\"\")\n}\n",
            Class::StateWord,
        );
        refused(
            "fn f(id: &str) -> bool {\n    let o = Command::new(\"bd\").args([\"show\", id]).output().unwrap();\n    String::from_utf8_lossy(&o.stdout).contains(\"CLOSED\")\n}\n",
            Class::StateWord,
        );
    }

    // ---- (c) identity parsed out of a unit name ------------------------------------------

    #[test]
    fn identity_from_a_unit_name_is_refused() {
        refused("fn f(u: &str) -> Option<&str> {\n    u.strip_prefix(\"spira-dwarf-\")\n}\n", Class::UnitName);
        refused("fn f(u: &str) -> &str {\n    u.trim_end_matches(\".service\")\n}\n", Class::UnitName);
        refused("fn f(units: &[String]) -> usize {\n    units.iter().filter(|u| u.starts_with(\"spira-dwarf-\")).count()\n}\n", Class::UnitName);
        refused("fn f(u: &str) -> Option<String> {\n    let re = Regex::new(r\"^spira-dwarf-(\\w+)\\.service$\").unwrap();\n    re.captures(u).map(|c| c[1].to_string())\n}\n", Class::UnitName);
    }

    // ---- what passes ---------------------------------------------------------------------

    #[test]
    fn a_typed_lc_read_passes() {
        passes("fn f(c: &LcClient, id: &str) -> bool {\n    match spira_store::lc_state::row_on(c, id) {\n        Ok(Some(r)) => r.state == \"READY\",\n        _ => false,\n    }\n}\n");
    }

    #[test]
    fn json_deserialised_into_a_struct_passes() {
        passes("fn f(id: &str) -> bool {\n    let o = Command::new(\"spira-lc\").args([\"show\", id, \"--json\"]).output().unwrap();\n    let text = String::from_utf8_lossy(&o.stdout);\n    let row: Row = serde_json::from_str(&text).unwrap();\n    if row.state == \"WORKING\" {\n        return true;\n    }\n    false\n}\n");
        passes("fn f(id: &str) -> Option<Row> {\n    let o = Command::new(\"spira-lc\").args([\"show\", id]).output().ok()?;\n    let rows = spira_store::lc_state::parse_show(&String::from_utf8_lossy(&o.stdout)).ok()?;\n    rows.filter(|r| r.state == \"READY\")\n}\n");
    }

    #[test]
    fn an_exit_code_enum_passes() {
        passes("fn f(id: &str) -> Answer {\n    let o = Command::new(\"spira-lc\").args([\"holds\", id]).output().unwrap();\n    let code = o.status.code();\n    if code == Some(0) {\n        Answer::Held\n    } else if !o.status.success() {\n        Answer::Unknown\n    } else {\n        Answer::Free\n    }\n}\n");
    }

    #[test]
    fn display_only_formatting_passes() {
        passes("fn f() {\n    let o = Command::new(\"gh\").args([\"pr\", \"list\"]).output().unwrap();\n    let s = String::from_utf8_lossy(&o.stdout);\n    for l in s.lines() {\n        println!(\"  {}\", l.trim());\n    }\n    let joined: Vec<String> = s.lines().map(|l| format!(\"> {l}\")).collect();\n    eprintln!(\"{}\", joined.join(\"\\n\"));\n}\n");
    }

    #[test]
    fn text_ops_on_values_that_are_not_output_pass() {
        passes("fn f(args: &[String]) {\n    let o = Command::new(\"spira-lc\").args([\"list\"]).status();\n    for a in args {\n        if a.starts_with(\"--\") {\n            flag(a);\n        }\n    }\n    let dir = run.join(\"queue\");\n    if dir.to_string_lossy().contains(\"x\") { go(); }\n}\n");
        passes("fn f(name: &str) -> bool {\n    name.contains(\"READY\")\n}\n");
    }

    #[test]
    fn building_a_unit_name_passes() {
        passes("fn f(id: &str) -> String {\n    format!(\"spira-dwarf-{id}.service\")\n}\n");
        passes("fn f(p: &str) -> bool {\n    p.ends_with(\".rs\") && p.starts_with(\"spira-lint/\")\n}\n");
        passes("fn f(file: &str) -> bool {\n    file.ends_with(\".service\") && file.starts_with(\"spira-\")\n}\n");
    }

    #[test]
    fn prose_and_literals_are_not_code() {
        passes("// if out.contains(\"cannot tell\") { u.strip_prefix(\"spira-dwarf-\") }\nfn f() -> &'static str {\n    \"text.contains(\\\"cannot tell\\\")\"\n}\n");
    }

    #[test]
    fn test_code_is_exempt() {
        let src = "fn f(e: &str) -> bool {\n    e.contains(\"cannot tell\")\n}\n";
        assert!(scan(src.as_bytes(), &|_| true).is_empty());
    }

    // ---- the allow file ------------------------------------------------------------------

    fn run(t: &TempDir, files: &[&str]) -> Vec<String> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        StateFromText.check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    const OFFENDER: &str = "fn f(e: &str) -> bool {\n    e.contains(\"cannot tell\")\n}\n";

    #[test]
    fn the_allow_list_only_shrinks() {
        let t = TempDir::new("state-from-text-allow");
        t.write("a/src/lib.rs", OFFENDER);
        t.write(ALLOW_FILE, "");
        let got = run(&t, &["a/src/lib.rs"]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with("state-from-text: a/src/lib.rs:2:"), "{got:?}");
        assert!(got[0].contains("typed"), "the refusal names its exit: {got:?}");
        t.write(ALLOW_FILE, "1 cannot-tell a/src/lib.rs\n");
        assert!(run(&t, &["a/src/lib.rs"]).is_empty());
        t.write(ALLOW_FILE, "1 output a/src/lib.rs\n");
        assert_eq!(run(&t, &["a/src/lib.rs"]).len(), 2, "a count in another class does not cover it");
        t.write(ALLOW_FILE, "2 cannot-tell a/src/lib.rs\n");
        let got = run(&t, &["a/src/lib.rs"]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("lower it to 1"), "{got:?}");
        t.write("a/src/lib.rs", "fn f() {}\n");
        t.write(ALLOW_FILE, "1 cannot-tell a/src/lib.rs\n");
        assert!(run(&t, &["a/src/lib.rs"])[0].contains("remove the line"));
    }

    #[test]
    fn a_malformed_allow_line_refuses() {
        assert!(parse_allow("x 1 output a.rs\n").is_err());
        assert!(parse_allow("1 nonsense a.rs\n").is_err());
        assert!(parse_allow("0 output a.rs\n").is_err());
        assert!(parse_allow("1 output a.rs\n1 output a.rs\n").is_err());
    }
}
