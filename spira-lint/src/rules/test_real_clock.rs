//! `test-real-clock` — test code does not sleep or compare `Instant::now()` / `.elapsed()` on
//! the real clock unless the line carries `clock-safe: <why>`. Contract: DESIGN.md.

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct TestRealClock;

const NAME: &str = "test-real-clock";
const MARKER: &str = "clock-safe:";
const SLEEP: &str = "test sleeps on the real clock: wait on a signal from the thing under test (channel, condvar, counter, marker file); else inject the clock (runtime::ports::VirtualClock or a test FakeClock); for scheduling, retries, timeouts or rounds across actors write the test in sim/. Where the clock only caps a wait for a named signal, mark `// clock-safe: <why>`";
const COMPARE: &str = "test asserts on a real-clock read: wait on a signal from the thing under test (channel, condvar, counter, marker file); else inject the clock (runtime::ports::VirtualClock or a test FakeClock); for scheduling, retries, timeouts or rounds across actors write the test in sim/. Where the clock only caps a wait for a named signal, mark `// clock-safe: <why>`";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

fn applies(path: &str) -> bool {
    path.ends_with(".rs") && !path.starts_with("target/") && !path.split('/').any(|c| c == "perf-checks" || c == "target")
}

type Hit = (usize, &'static str);

fn matching(code: &[u8], open: usize) -> Option<usize> {
    let (o, c) = (code[open], if code[open] == b'{' { b'}' } else { b')' });
    let mut depth = 0usize;
    for (i, b) in code.iter().enumerate().skip(open) {
        if *b == o {
            depth += 1;
        } else if *b == c {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// The innermost `fn` whose body holds `at`: where its attributes begin, and its body's span.
fn enclosing_fn(code: &[u8], at: usize) -> Option<(usize, usize, usize)> {
    static FN: OnceLock<Regex> = OnceLock::new();
    let fns: Vec<_> = re(&FN, r"\bfn\s+[A-Za-z_]\w*").find_iter(code).take_while(|m| m.start() < at).collect();
    for m in fns.iter().rev() {
        let Some(open) = code[m.end()..].iter().position(|b| *b == b'{' || *b == b';').map(|p| m.end() + p) else { continue };
        if code[open] != b'{' {
            continue;
        }
        if let Some(close) = matching(code, open) {
            if close > at {
                let attrs = code[..m.start()].iter().rposition(|b| matches!(*b, b'}' | b';')).map_or(0, |p| p + 1);
                return Some((attrs, open, close));
            }
        }
    }
    None
}

fn hits(src: &[u8], regions: &[(usize, usize)], whole: bool) -> Vec<Hit> {
    static SLEEP_RE: OnceLock<Regex> = OnceLock::new();
    static CLOCK: OnceLock<Regex> = OnceLock::new();
    static CMP: OnceLock<Regex> = OnceLock::new();
    static BIND: OnceLock<Regex> = OnceLock::new();
    static ASSERT: OnceLock<Regex> = OnceLock::new();
    static PAUSED: OnceLock<Regex> = OnceLock::new();
    static PAUSE_CALL: OnceLock<Regex> = OnceLock::new();
    let sleep = re(&SLEEP_RE, r"\bsleep\s*\(");
    let clock = re(&CLOCK, r"Instant::now\(\)|SystemTime::now\(\)|\.elapsed\(\)");
    let cmp = re(&CMP, r"[^-=<>]\s*(?:<=?|>=?)\s*[^=<>]");
    let bind = re(&BIND, r"\blet\s+(?:mut\s+)?([A-Za-z_]\w*)\s*(?::[^=;]*)?=([^;]*);");
    let assert = re(&ASSERT, r"\bassert\w*!\s*\(");
    let paused = re(&PAUSED, r"start_paused\s*=\s*true");
    let pause_call = re(&PAUSE_CALL, r"\btime::pause\s*\(\s*\)");
    let cl = rust::classify(src);
    let code = cl.code_only();
    let in_test = |at: usize| whole || regions.iter().any(|(a, b)| *a <= at && at < *b);
    let line_bounds = |at: usize| {
        let ls = code[..at].iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
        let le = code[at..].iter().position(|b| *b == b'\n').map_or(code.len(), |p| at + p);
        (ls, le)
    };
    let marked = |at: usize| {
        let ls = src[..at].iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
        let prev = src[..ls.saturating_sub(1)].iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
        let end = src[at..].iter().position(|b| *b == b'\n').map_or(src.len(), |p| at + p);
        src[prev..end].windows(MARKER.len()).any(|w| w == MARKER.as_bytes())
    };
    let paused_fn = |at: usize| {
        enclosing_fn(&code, at)
            .is_some_and(|(attrs, open, close)| paused.is_match(&code[attrs..open]) || pause_call.is_match(&code[open..close]))
    };
    let mut out = Vec::new();
    for m in sleep.find_iter(&code) {
        let at = m.start();
        if in_test(at) && !marked(at) && !code[..at].ends_with(b"fn ") && !paused_fn(at) {
            out.push((cl.line_of(at), SLEEP));
        }
    }
    for m in clock.find_iter(&code) {
        let at = m.start();
        let (ls, le) = line_bounds(at);
        if in_test(at) && !marked(at) && cmp.is_match(&code[ls..le]) {
            out.push((cl.line_of(at), COMPARE));
        }
    }
    for caps in bind.captures_iter(&code) {
        let whole_m = caps.get(0).expect("match");
        if !in_test(whole_m.start()) || !clock.is_match(&caps[2]) {
            continue;
        }
        let ident = String::from_utf8_lossy(&caps[1]).into_owned();
        let word = Regex::new(&format!(r"\b{}\b", regex::escape(&ident))).expect("ident regex");
        let end = enclosing_fn(&code, whole_m.start()).map_or(code.len(), |(_, _, close)| close);
        for a in assert.find_iter(&code[whole_m.end()..end]) {
            let open = whole_m.end() + a.end() - 1;
            let Some(close) = matching(&code, open) else { continue };
            let args = &code[open..close];
            if word.is_match(args) && cmp.is_match(args) && !marked(open) {
                out.push((cl.line_of(open), COMPARE));
            }
        }
    }
    out.sort();
    out.dedup_by_key(|h| h.0);
    out
}

impl Rule for TestRealClock {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        None
    }

    fn applies_to(&self, e: &Entry) -> bool {
        applies(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let mut out = Vec::new();
        for e in &files {
            let Some(src) = tree.content(e) else { continue };
            let s = shape(src);
            let whole = whole_test_files(tree, &[(*e, shape(src))]).contains(&e.path);
            for (line, msg) in hits(src, &s.regions, whole) {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: msg.to_string() });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "A test that sleeps or races the real clock passes on an idle box and flips under load, \
ejecting a round member that never touched the code. First look for a signal from the thing under \
test (channel, condvar, counter, marker file) and wait on it. Else inject the clock \
(runtime::ports::VirtualClock, or a test FakeClock). For scheduling, retries, timeouts or rounds \
across actors, write the test in sim/. Only where the real clock caps a wait for a named signal, \
mark the line `// clock-safe: <why>`."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(TestRealClock)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        TestRealClock.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_test_sleep_and_a_clock_comparison_are_flagged() {
        let t = TempDir::new("trc-fail");
        t.write(
            "a/src/lib.rs",
            "pub fn f() { std::thread::sleep(D); }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        std::thread::sleep(D);\n        let t0 = Instant::now();\n        if t0.elapsed() > LIMIT { panic!(); }\n    }\n}\n",
        );
        assert_eq!(
            run(&t, &["a/src/lib.rs"]).unwrap(),
            vec![format!("test-real-clock: a/src/lib.rs:6: {SLEEP}"), format!("test-real-clock: a/src/lib.rs:8: {COMPARE}")]
        );
    }

    #[test]
    fn marked_lines_production_code_and_plain_clock_reads_pass() {
        let t = TempDir::new("trc-pass");
        t.write(
            "a/src/lib.rs",
            "pub fn f() { std::thread::sleep(D); }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        // clock-safe: polls a marker file under a cap\n        std::thread::sleep(D);\n        let t0 = Instant::now();\n        use_it(t0);\n    }\n}\n",
        );
        t.write("a/perf-checks/p.rs", "fn t() { sleep(D); }\n");
        assert_eq!(run(&t, &["a/src/lib.rs", "a/perf-checks/p.rs"]).unwrap(), Vec::<String>::new());
    }

    fn one(body: &str) -> Vec<String> {
        let t = TempDir::new("trc-one");
        t.write("a/src/lib.rs", &format!("#[cfg(test)]\nmod tests {{\n{body}\n}}\n"));
        run(&t, &["a/src/lib.rs"]).unwrap()
    }

    #[test]
    fn a_system_time_bound_then_asserted_on_flags_the_sleep_and_the_assert() {
        let got = one("#[test]\nfn t() {\n    let t0 = SystemTime::now();\n    std::thread::sleep(Duration::from_millis(50));\n    let took = t0.elapsed().unwrap();\n    assert!(took >= Duration::from_millis(50));\n}");
        assert_eq!(
            got,
            vec![
                format!("test-real-clock: a/src/lib.rs:6: {SLEEP}"),
                format!("test-real-clock: a/src/lib.rs:8: {COMPARE}"),
            ]
        );
    }

    #[test]
    fn a_retry_loop_that_sleeps_between_attempts_is_flagged() {
        let got = one("#[test]\nfn t() {\n    for _ in 0..3 { attempt(); std::thread::sleep(D); }\n    assert_eq!(calls(), 2);\n}");
        assert_eq!(got, vec![format!("test-real-clock: a/src/lib.rs:5: {SLEEP}")]);
    }

    #[test]
    fn the_same_behaviour_on_an_injected_clock_or_a_signal_passes() {
        let got = one("#[test]\nfn t() {\n    let clock = VirtualClock::new();\n    let t0 = clock.now();\n    clock.advance(50);\n    assert!(clock.now() - t0 >= 50);\n}\n#[test]\nfn u() {\n    let (tx, rx) = channel();\n    spawn(move || tx.send(()));\n    rx.recv().unwrap();\n}");
        assert_eq!(got, Vec::<String>::new());
    }

    #[test]
    fn a_clock_read_that_only_stamps_fixture_data_passes() {
        let got = one("#[test]\nfn t() {\n    let now = SystemTime::now();\n    set_mtime(&p, now);\n    assert!(p.exists());\n}");
        assert_eq!(got, Vec::<String>::new());
    }

    #[test]
    fn a_sleep_under_a_paused_tokio_clock_passes() {
        let got = one("#[tokio::test(start_paused = true)]\nasync fn t() {\n    tokio::time::sleep(D).await;\n}\n#[tokio::test]\nasync fn u() {\n    tokio::time::pause();\n    tokio::time::sleep(D).await;\n}");
        assert_eq!(got, Vec::<String>::new());
        let unpaused = one("#[tokio::test]\nasync fn t() {\n    tokio::time::sleep(D).await;\n}");
        assert_eq!(unpaused, vec![format!("test-real-clock: a/src/lib.rs:5: {SLEEP}")]);
    }

    #[test]
    fn a_clock_safe_marker_exempts_a_sleep_and_a_bound_assert() {
        let got = one("#[test]\nfn t() {\n    // clock-safe: polls the marker file the child writes\n    std::thread::sleep(D);\n    let t0 = Instant::now();\n    // clock-safe: cap on the wait for the marker\n    assert!(t0.elapsed() < CAP);\n}");
        assert_eq!(got, Vec::<String>::new());
    }

    #[test]
    fn the_message_names_the_sim_crate_and_the_virtual_clock() {
        for m in [SLEEP, COMPARE, TestRealClock.hint()] {
            assert!(m.contains("sim/") && m.contains("VirtualClock"), "{m}");
        }
    }

    #[test]
    fn no_candidate_files_is_a_refusal() {
        let t = TempDir::new("trc-empty");
        t.write("a.txt", "x\n");
        assert_eq!(run(&t, &["a.txt"]), Err(LintError::EmptyScope));
    }
}
