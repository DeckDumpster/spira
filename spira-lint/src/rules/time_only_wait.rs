//! `time-only-wait` — a wait or gate names the object or signal it waits on and carries a cap;
//! a bare time window is refused (law-a-wait-names-what-it-waits-for). Contract: DESIGN.md.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct TimeOnlyWait;

const NAME: &str = "time-only-wait";
const ALLOW_FILE: &str = "spira-lint/time-only-wait-allow";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

fn is_test_path(path: &str) -> bool {
    path.split('/').any(|c| c == "tests" || c == "testlib" || c == "perf-checks" || c.starts_with("test-") || c == "target")
}

type Hit = (usize, &'static str);

const SLEEP_THEN_PROCEED: &str = "sleep outside any polling loop: the code proceeds on the clock, not on the thing it waits for";
const COUNTED_SLEEP: &str = "loop of sleeps with no exit on a condition: it waits a fixed time and proceeds";
const UNCAPPED_POLL: &str = "polling loop with no cap (deadline, attempt count or timeout) in sight";
const EXPIRY: &str = "a gate or stamp that lifts by the clock: name the object it waits on and keep the time only as a cap";
const MARKER: &str = "waits-on:";

fn shell_hits(src: &[u8]) -> Vec<Hit> {
    static SLEEP: OnceLock<Regex> = OnceLock::new();
    static TICK: OnceLock<Regex> = OnceLock::new();
    static EXIT: OnceLock<Regex> = OnceLock::new();
    static CAP: OnceLock<Regex> = OnceLock::new();
    static EXP: OnceLock<Regex> = OnceLock::new();
    static HEREDOC: OnceLock<Regex> = OnceLock::new();
    static HEAD: OnceLock<Regex> = OnceLock::new();
    let loop_head = re(&HEAD, r"(?:^\s*|[;&|]\s*)(?:while|until|for)\s");
    let sleep = re(&SLEEP, r"(?:^|[;&|({]\s*|\s)sleep\s+[0-9$\x22\x27{]");
    let tick = re(&TICK, r"^\s*while\s+(?:true\b|:|\[\s*1\s*\]|\[\[\s*1\s*\]\])");
    let exit = re(&EXIT, r"\b(?:break|return|exit)\b");
    let cap = re(&CAP, r"(?i)deadline|timeout|SECONDS|date \+%s|tries|attempt|retr|max|limit|ceiling|waited|elapsed|\b_?w\b|\(\(|-lt\b|-ge\b|-le\b|-gt\b|\bn\b");
    let exp = re(&EXP, r#"(?:["']|\\n|^)\s*expires[ =:]*["']?\s*(?:\{|\$|%)|(?i:\b(?:drain|gate|hold|pause|stamp)\w*_ttl\b)"#);
    let heredoc = re(&HEREDOC, r#"<<-?\s*['"]?([A-Za-z_][A-Za-z0-9_]*)['"]?"#);

    struct Frame {
        header: String,
        body: String,
        sleeps: Vec<usize>,
    }
    let mut out = Vec::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut ending: Option<String> = None;
    let mut prev_marked = false;
    let judge = |f: &Frame, out: &mut Vec<Hit>| {
        if f.sleeps.is_empty() || tick.is_match(f.header.as_bytes()) {
            return;
        }
        let text = format!("{}\n{}", f.header, f.body);
        let msg = if f.header.trim_start().starts_with("for") {
            if exit.is_match(text.as_bytes()) { return } else { COUNTED_SLEEP }
        } else if cap.is_match(text.as_bytes()) {
            return;
        } else {
            UNCAPPED_POLL
        };
        out.extend(f.sleeps.iter().map(|l| (*l, msg)));
    };

    for (i, raw) in crate::lines(src).iter().enumerate() {
        let n = i + 1;
        let line = String::from_utf8_lossy(raw).into_owned();
        if let Some(tag) = &ending {
            if line.trim() == tag {
                ending = None;
            }
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            prev_marked = trimmed.contains(MARKER);
            continue;
        }
        let marked = prev_marked || line.contains(MARKER);
        prev_marked = false;
        let code = match line.find(" #") {
            Some(p) => &line[..p],
            None => &line[..],
        };
        if let Some(c) = heredoc.captures(code.as_bytes()) {
            ending = Some(String::from_utf8_lossy(&c[1]).into_owned());
        }
        if let Some(m) = loop_head.find(code.as_bytes()) {
            frames.push(Frame { header: code[m.start()..].trim_start_matches([';', '&', '|', ' ']).to_string(), body: String::new(), sleeps: Vec::new() });
        }
        if exp.is_match(code.as_bytes()) && !marked {
            out.push((n, EXPIRY));
        }
        if sleep.is_match(code.as_bytes()) && !marked {
            match frames.last_mut() {
                Some(f) => f.sleeps.push(n),
                None => out.push((n, SLEEP_THEN_PROCEED)),
            }
        }
        for f in frames.iter_mut() {
            f.body.push_str(code);
            f.body.push('\n');
        }
        let dones = code.split(|c: char| c.is_whitespace() || c == ';').filter(|w| *w == "done").count();
        for _ in 0..dones {
            if let Some(f) = frames.pop() {
                judge(&f, &mut out);
            }
        }
    }
    out.sort();
    out
}

fn rust_hits(src: &[u8], test_regions: &[(usize, usize)], whole: bool) -> Vec<Hit> {
    static SLEEP: OnceLock<Regex> = OnceLock::new();
    static LOOP: OnceLock<Regex> = OnceLock::new();
    static EXP: OnceLock<Regex> = OnceLock::new();
    static PASS: OnceLock<Regex> = OnceLock::new();
    static SEAM: OnceLock<Regex> = OnceLock::new();
    let sleep = re(&SLEEP, r"\bsleep\s*\(");
    let passthrough = re(&PASS, r"^\bsleep\s*\((?:d|dur|duration|ms|secs|s)\s*\)");
    let closure_seam = re(&SEAM, r"\|[^|]*\|\s*(?:std::)?(?:thread::)?$");
    let line_start = |c: &[u8], at: usize| c[..at].iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
    let looph = re(&LOOP, r"\bloop\s*$|^\s*(?:'\w+:\s*)?(?:while|for)\b|\bfn\s+(?:sleep\w*|wait)\b|\(0\.\.|\.(?:any|all|find|take|for_each)\(\|");
    let exp = re(&EXP, r#"(?:["']|\\n)expires[ =:]*\{|(?i:\b(?:drain|gate|hold|pause|stamp)\w*_ttl\b)"#);
    let cl = rust::classify(src);
    let code = cl.code_only();
    let text = cl.without_comments();
    let in_test = |at: usize| whole || test_regions.iter().any(|(a, b)| *a <= at && at < *b);
    let mut out = Vec::new();
    let marked = |at: usize| {
        let ls = src[..at].iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
        let prev = src[..ls.saturating_sub(1)].iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
        let end = src[at..].iter().position(|b| *b == b'\n').map_or(src.len(), |p| at + p);
        src[prev..end].windows(MARKER.len()).any(|w| w == MARKER.as_bytes())
    };

    for m in sleep.find_iter(&code) {
        let at = m.start();
        if in_test(at) || marked(at) || code[..at].ends_with(b"fn ") || passthrough.is_match(&code[at..]) || closure_seam.is_match(&code[line_start(&code, at)..at]) {
            continue;
        }
        let mut depth = 0i32;
        let mut in_loop = false;
        let mut j = at;
        while j > 0 {
            j -= 1;
            match code[j] {
                b'}' => depth += 1,
                b'{' if depth > 0 => depth -= 1,
                b'{' => {
                    let from = code[..j].iter().rposition(|b| matches!(b, b';' | b'{' | b'}')).map_or(0, |p| p + 1);
                    if looph.is_match(&code[from..j]) {
                        in_loop = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !in_loop {
            out.push((cl.line_of(at), SLEEP_THEN_PROCEED));
        }
    }
    for m in exp.find_iter(&text) {
        let at = m.start();
        if !in_test(at) && !marked(at) {
            out.push((cl.line_of(at), EXPIRY));
        }
    }
    out.sort();
    out.dedup();
    out
}

fn parse_allow(text: &str) -> Result<BTreeMap<String, (usize, usize)>, LintError> {
    let mut m = BTreeMap::new();
    for (i, l) in text.lines().enumerate() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = t.split_whitespace().collect();
        let ok = f.len() == 3 && f[0].parse::<usize>().is_ok() && f[2].starts_with("sp-");
        if !ok {
            return Err(LintError::BadAllow {
                file: ALLOW_FILE.to_string(),
                line: i + 1,
                reason: format!("want `<count> <path> <converting-bead>`, got `{t}`"),
            });
        }
        m.insert(f[1].to_string(), (f[0].parse().unwrap_or(0), i + 1));
    }
    Ok(m)
}

fn all_hits(tree: &Tree, files: &[&Entry]) -> BTreeMap<String, Vec<Hit>> {
    let shapes: Vec<(&Entry, crate::rust_test::Shape)> =
        files.iter().filter(|e| e.path.ends_with(".rs")).filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
    let whole = whole_test_files(tree, &shapes);
    let mut by: BTreeMap<String, Vec<Hit>> = BTreeMap::new();
    for e in files {
        let Some(src) = tree.content(e) else { continue };
        let hits = if e.path.ends_with(".rs") {
            let regions = shapes.iter().find(|(x, _)| x.path == e.path).map(|(_, s)| s.regions.clone()).unwrap_or_default();
            rust_hits(src, &regions, whole.contains(&e.path))
        } else {
            shell_hits(src)
        };
        if !hits.is_empty() {
            by.insert(e.path.clone(), hits);
        }
    }
    by
}

impl Rule for TimeOnlyWait {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        (e.path.ends_with(".sh") || e.path.ends_with(".rs")) && !is_test_path(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let allow = parse_allow(&tree.read_text(ALLOW_FILE))?;
        let by = all_hits(tree, &files);
        let mut out = Vec::new();
        for (path, hits) in &by {
            let allowed = allow.get(path).map_or(0, |a| a.0);
            if hits.len() <= allowed {
                continue;
            }
            let note = if allowed == 0 { String::new() } else { format!(" ({} allowed, {} found)", allowed, hits.len()) };
            for (line, msg) in hits {
                out.push(Finding { rule: NAME, path: path.clone(), line: Some(*line), message: format!("{msg}{note}") });
            }
        }
        for (path, (n, line)) in &allow {
            let now = by.get(path).map_or(0, Vec::len);
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
        "Wait on a named object or signal (a file, pid, bead, row) in a loop bounded by a cap, or mark \
the line `# waits-on: <object>; cap <bound>` where the wait is genuinely that. A sleep-then-proceed, \
a stamp that expires by the clock, a fixed window assumed to be enough: each is a guess about the \
thing you could observe. spira-lint/time-only-wait-allow only shrinks and names the converting bead."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        TimeOnlyWait.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_sleep_then_proceed_and_an_expiring_stamp_are_flagged() {
        let t = TempDir::new("tow-fail");
        t.write(ALLOW_FILE, "");
        t.write("spira/go.sh", "#!/bin/bash\nstart_drain\nsleep 30\nproceed\n");
        t.write("spira/stamp.sh", "#!/bin/bash\nprintf 'now\\nexpires %s\\n' \"$(( $(date +%s) + 1800 ))\" > \"$stamp\"\n");
        t.write("spira/count.sh", "#!/bin/bash\nfor i in 1 2 3; do\n  sleep 10\ndone\nproceed\n");
        t.write("a/src/lib.rs", "pub fn f() {\n    std::thread::sleep(std::time::Duration::from_secs(5));\n}\n");
        let got = run(&t, &["spira/go.sh", "spira/stamp.sh", "spira/count.sh", "a/src/lib.rs"]).unwrap();
        assert_eq!(
            got,
            vec![
                format!("time-only-wait: a/src/lib.rs:2: {SLEEP_THEN_PROCEED}"),
                format!("time-only-wait: spira/count.sh:3: {COUNTED_SLEEP}"),
                format!("time-only-wait: spira/go.sh:3: {SLEEP_THEN_PROCEED}"),
                format!("time-only-wait: spira/stamp.sh:2: {EXPIRY}"),
            ]
        );
    }

    #[test]
    fn a_capped_poll_on_a_named_condition_ticks_markers_and_tests_pass() {
        let t = TempDir::new("tow-pass");
        t.write(ALLOW_FILE, "");
        t.write("spira/poll.sh", "#!/bin/bash\nfor _i in $(seq 30); do\n  [ -f \"$ready\" ] && break\n  sleep 1\ndone\nuntil [ -f \"$f\" ] || [ \"$SECONDS\" -gt \"$deadline\" ]; do sleep 1; done\n");
        t.write("spira/tick.sh", "#!/bin/bash\nwhile true; do\n  work\n  sleep 60\ndone\n");
        t.write("spira/marked.sh", "#!/bin/bash\n# waits-on: the tmux pane to repaint; cap 1s\nsleep 1\nsleep 2 # waits-on: socket; cap 2s\n");
        t.write("spira/test-x.sh", "sleep 5\n");
        t.write(
            "a/src/lib.rs",
            "pub fn f(d: Deadline) {\n    loop {\n        if ready() || d.passed() { break; }\n        std::thread::sleep(POLL);\n    }\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { std::thread::sleep(D); }\n}\n",
        );
        let files = ["spira/poll.sh", "spira/tick.sh", "spira/marked.sh", "spira/test-x.sh", "a/src/lib.rs"];
        assert_eq!(run(&t, &files).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn an_uncapped_poll_is_flagged() {
        let t = TempDir::new("tow-uncapped");
        t.write(ALLOW_FILE, "");
        t.write("spira/w.sh", "#!/bin/bash\nwhile kill -0 \"$pid\" 2>/dev/null; do\n  sleep 1\ndone\n");
        assert_eq!(run(&t, &["spira/w.sh"]).unwrap(), vec![format!("time-only-wait: spira/w.sh:3: {UNCAPPED_POLL}")]);
    }

    #[test]
    fn a_counted_allowance_passes_and_a_stale_or_malformed_one_is_refused() {
        let t = TempDir::new("tow-allow");
        t.write("spira/old.sh", "sleep 3\n");
        t.write("spira/fixed.sh", "true\n");
        t.write(ALLOW_FILE, "# h\n1 spira/old.sh sp-abc\n1 spira/fixed.sh sp-abc\n");
        assert_eq!(
            run(&t, &["spira/old.sh", "spira/fixed.sh"]).unwrap(),
            vec!["time-only-wait: spira-lint/time-only-wait-allow:3: allows 1 for spira/fixed.sh, which now has 0 — remove the line (the list only shrinks)"]
        );
        t.write(ALLOW_FILE, "1 spira/old.sh\n");
        assert!(run(&t, &["spira/old.sh"]).is_err());
    }

    #[test]
    fn no_candidate_files_is_a_refusal() {
        let t = TempDir::new("tow-empty");
        t.write("a.txt", "x\n");
        assert_eq!(run(&t, &["a.txt"]), Err(LintError::EmptyScope));
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(TimeOnlyWait)]
}
