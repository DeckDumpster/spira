//! `loop-blocking` — nothing that blocks runs on the runtime's event loop or in its handlers
//! (law-an-event-loop-never-blocks). Contract and known limits: DESIGN.md.
//!
//! A match is exempt inside the arguments of an `offload(…)` call, or on a line marked
//! `// loop-safe: <why>` (or under an unbroken comment block that carries it). The marker is
//! read from comment bytes only. A handler module takes neither exit.
//!
//! Existing violations are counted per file in `spira-lint/loop-blocking-allow`, which only
//! shrinks.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::bytes::Regex;

use super::call_deadline::{fn_spans, is_marked, marked_lines, trimmed};
use crate::lex::rust::{self, Class};
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct LoopBlocking;

const NAME: &str = "loop-blocking";
const ALLOW_FILE: &str = "spira-lint/loop-blocking-allow";
const OFFLOAD_FILE: &str = "runtime/src/offload.rs";
const LOOP_MODULES: &[&str] = &["runtime/src/host.rs", "spira-runtime/src/notify.rs", "spira-runtime/src/main.rs"];
const HANDLER_MODULES: &[&str] = &["spira-runtime/src/watchers/", "runtime/src/heartbeat.rs", "runtime/src/engine.rs"];

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

fn marker_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"loop-safe:\s*\S")
}

pub struct Hit {
    pub line: usize,
    pub message: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Loop,
    Handler,
    Other,
}

fn kind_of(path: &str) -> Kind {
    if LOOP_MODULES.contains(&path) {
        Kind::Loop
    } else if HANDLER_MODULES.iter().any(|h| if h.ends_with('/') { path.starts_with(h) } else { path == *h }) {
        Kind::Handler
    } else {
        Kind::Other
    }
}

fn in_scope(path: &str) -> bool {
    (path.starts_with("runtime/src/") || path.starts_with("spira-runtime/src/")) && path.ends_with(".rs")
}

/// The index of the bracket closing the one at `open`, over code-only bytes.
fn close_of(code: &[u8], open: usize) -> usize {
    let (o, c) = if code[open] == b'(' { (b'(', b')') } else { (b'{', b'}') };
    let mut depth = 0i32;
    for (i, &b) in code.iter().enumerate().skip(open) {
        if b == o {
            depth += 1;
        } else if b == c {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
    }
    code.len()
}

/// Byte ranges of every `async fn` body and `async {}` / `async move {}` block.
fn async_ranges(code: &[u8]) -> Vec<(usize, usize)> {
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    let mut out = Vec::new();
    for f in fn_spans(code) {
        let before = code[..f.start].trim_ascii_end();
        let before = before.strip_suffix(b"unsafe").map_or(before, |b| b.trim_ascii_end());
        if before.ends_with(b"async") && !before[..before.len() - 5].last().is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
            out.push((f.start, f.end));
        }
    }
    for m in re(&BLOCK, r"\basync\s+(?:move\s+)?\{").find_iter(code) {
        let open = m.end() - 1;
        out.push((open, close_of(code, open)));
    }
    out
}

/// Byte ranges of the argument list of every `offload(…)` call.
fn offload_ranges(code: &[u8]) -> Vec<(usize, usize)> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"\boffload\s*\(").find_iter(code).map(|m| (m.end() - 1, close_of(code, m.end() - 1))).collect()
}

fn within(ranges: &[(usize, usize)], at: usize) -> bool {
    ranges.iter().any(|(a, b)| *a <= at && at < *b)
}

/// The path segments written directly before `at`, e.g. `tokio::net::` for `tokio::net::TcpStream`.
fn path_before(code: &[u8], at: usize) -> &[u8] {
    let from = code[..at].iter().rposition(|b| !(b.is_ascii_alphanumeric() || *b == b'_' || *b == b':')).map_or(0, |p| p + 1);
    &code[from..at]
}

fn is_tokio(code: &[u8], at: usize) -> bool {
    path_before(code, at).starts_with(b"tokio::")
}

fn blocking_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"\bstd::fs\b|\bfs::|\bFile::(?:open|create)\b|\bOpenOptions\b|\bread_dir\b|\bstd::net\b|\b(?:Tcp|Udp|Unix)Stream::|\bUdpSocket::|\bUnix(?:Datagram|Listener)::|\bsend_to_addr\b|\bstd::process\b|\bCommand::new\b|\bthread::sleep\b|\bflock\b|\bureq::|\blc_call\s*\(|\bblock_on\s*\(|\be?println!|\bstd::io::stdin\b",
    )
}

fn lock_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"\.lock\s*\(\s*\)")
}

fn port_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"\.(?:run_until_idle|pump|tick)\s*\(|\bengine\s*\.\s*(?:submit|complete)\s*\(|\bExecutor::(?:submit|complete)\s*\(|\bEngine::open\s*\(|\bopen_engine\s*\(|\.with_marker\s*\(|\bSettings::load\s*\(",
    )
}

fn spawn_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"\bspawn_blocking\s*\(")
}

fn word(code: &[u8], m: regex::bytes::Match<'_>) -> String {
    String::from_utf8_lossy(&code[m.start()..m.end()]).trim().trim_end_matches('(').to_string()
}

/// Every loop-blocking violation in one Rust source at `path`; `in_test` says whether a byte
/// offset is test code, which is exempt.
pub fn scan(path: &str, src: &[u8], in_test: &dyn Fn(usize) -> bool) -> Vec<Hit> {
    let kind = kind_of(path);
    let cl = rust::classify(src);
    let code = cl.code_only();
    let asyncs = async_ranges(&code);
    let offloads = offload_ranges(&code);
    let lines: Vec<&[u8]> = src.split(|b| *b == b'\n').collect();
    let marked = marked_lines(src, marker_re(), |i| cl.class[i] == Class::Comment);
    let skip_above = |t: &[u8]| {
        let t = trimmed(t);
        t.starts_with(b"//") || t.starts_with(b"/*") || t.starts_with(b"*")
    };
    let is_use = |at: usize| {
        let from = code[..at].iter().rposition(|b| matches!(b, b';' | b'{' | b'}')).map_or(0, |p| p + 1);
        let stmt = code[from..at].trim_ascii_start();
        stmt.starts_with(b"use ") || stmt.starts_with(b"pub use ") || stmt.starts_with(b"pub(crate) use ")
    };

    let on_loop = |at: usize| kind != Kind::Other || within(&asyncs, at);
    let exempt = |at: usize| {
        kind != Kind::Handler && (within(&offloads, at) || is_marked(&lines, &marked, cl.line_of(at), skip_above))
    };

    let mut seen: BTreeSet<(usize, String)> = BTreeSet::new();
    let mut out = Vec::new();
    let mut add = |at: usize, what: String, why: &str| {
        let line = cl.line_of(at);
        if seen.insert((line, what.clone())) {
            out.push(Hit { line, message: format!("{what} {why}") });
        }
    };

    for m in blocking_re().find_iter(&code) {
        let at = m.start();
        if in_test(at) || !on_loop(at) || exempt(at) || is_tokio(&code, at) || is_use(at) {
            continue;
        }
        add(at, format!("`{}`", word(&code, m)), "blocks the event loop");
    }
    for m in lock_re().find_iter(&code) {
        let at = m.start();
        if in_test(at) || !on_loop(at) || exempt(at) || code[m.end()..].trim_ascii_start().starts_with(b".await") {
            continue;
        }
        add(at, "`.lock()`".to_string(), "blocks the event loop");
    }
    if kind != Kind::Handler {
        for m in port_re().find_iter(&code) {
            let at = m.start();
            if in_test(at) || !on_loop(at) || exempt(at) {
                continue;
            }
            add(at, format!("`{}`", word(&code, m)), "is an engine or port call on the event loop");
        }
    }
    if path != OFFLOAD_FILE {
        for m in spawn_re().find_iter(&code) {
            let at = m.start();
            if in_test(at) || exempt(at) || is_use(at) {
                continue;
            }
            add(at, "`spawn_blocking`".to_string(), "outside `runtime::offload`");
        }
    }
    out.sort_by_key(|h| h.line);
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
        let n = f.first().and_then(|n| n.parse::<usize>().ok()).filter(|n| *n > 0);
        let (Some(n), true) = (n, f.len() == 3 && f[2].starts_with("sp-")) else {
            return Err(LintError::BadAllow {
                file: ALLOW_FILE.to_string(),
                line: i + 1,
                reason: format!("want `<count> <path> <converting-bead>` with a count above 0, got `{t}`"),
            });
        };
        if m.insert(f[1].to_string(), (n, i + 1)).is_some() {
            return Err(LintError::BadAllow { file: ALLOW_FILE.to_string(), line: i + 1, reason: "path listed twice".to_string() });
        }
    }
    Ok(m)
}

/// Every violation in the walk, before the allow list.
pub fn violations(tree: &Tree) -> Vec<(String, Hit)> {
    let files: Vec<&Entry> = tree.entries.iter().filter(|e| in_scope(&e.path)).collect();
    let shapes: Vec<(&Entry, Shape)> = files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
    let whole = whole_test_files(tree, &shapes);
    let mut out = Vec::new();
    for (e, s) in &shapes {
        if whole.contains(&e.path) {
            continue;
        }
        let Some(src) = tree.content(e) else { continue };
        let hits = scan(&e.path, src, &|at| s.regions.iter().any(|(a, b)| *a <= at && at < *b));
        out.extend(hits.into_iter().map(|h| (e.path.clone(), h)));
    }
    out.sort_by(|a, b| (&a.0, a.1.line).cmp(&(&b.0, b.1.line)));
    out
}

/// The allow file's text for the tree as it stands: what `--emit-allow` prints, naming `bead`.
pub fn render_allow(tree: &Tree, bead: &str) -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (p, _) in violations(tree) {
        *counts.entry(p).or_default() += 1;
    }
    let mut s = String::from(
        "# loop-blocking: files that block the event loop and predate the rule, as\n\
# `<count> <path> <converting-bead>`. Generated: spira-lint --only loop-blocking --emit-allow.\n\
#\n\
# SHRINK-ONLY. A count goes down as the call moves into `runtime::offload`; nothing is\n\
# raised for a NEW call.\n",
    );
    for (p, n) in counts {
        s.push_str(&format!("{n} {p} {bead}\n"));
    }
    s
}

impl Rule for LoopBlocking {
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
        for (p, h) in violations(tree) {
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
                let why = if tree.entry(path).is_none() { "which does not exist".to_string() } else { format!("which now has {now}") };
                let fix = if now == 0 { "remove the line".to_string() } else { format!("lower it to {now}") };
                out.push(Finding {
                    rule: NAME,
                    path: ALLOW_FILE.to_string(),
                    line: Some(*line),
                    message: format!("allows {n} for {path}, {why} — {fix} (the list only shrinks)"),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "a blocking call on the event loop stops every turn, the watchdog kick included \
(law-an-event-loop-never-blocks). Wrap the call in `runtime::offload` and await it under a deadline, \
or move the I/O behind a port that the offloaded engine calls. A line that really cannot block carries \
`// loop-safe: <why>`; a handler module takes no exemption. Existing violations are counted in \
spira-lint/loop-blocking-allow, which only shrinks."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(LoopBlocking)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(files: &[(&str, &str)], allow: Option<&str>) -> Vec<String> {
        let t = TempDir::new("loopblk");
        t.git_init();
        for (p, c) in files {
            t.write(p, c);
        }
        if let Some(a) = allow {
            t.write(ALLOW_FILE, a);
        }
        t.git(&["add", "."]);
        let tree = Tree::from_git(t.path()).unwrap();
        LoopBlocking.check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    const HANDLER: &str = "spira-runtime/src/watchers/round_duty.rs";

    fn handler_with(body: &str) -> String {
        format!("pub fn handle() {{\n{body}\n}}\n")
    }

    #[test]
    fn a_plant_in_a_handler_is_caught_and_withdrawn() {
        let planted = handler_with("    let _ = std::fs::read(\"/etc/hostname\");");
        let got = run(&[(HANDLER, &planted)], None);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains(HANDLER) && got[0].contains(":2"), "{got:?}");
        assert!(run(&[(HANDLER, &handler_with("    let _ = 1;"))], None).is_empty());
    }

    #[test]
    fn the_real_handler_with_a_plant_is_caught() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../spira-runtime/src/watchers/round_duty.rs");
        let real = std::fs::read_to_string(path).unwrap();
        assert!(run(&[(HANDLER, &real)], None).is_empty());
        let head = "fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {\n";
        assert!(real.contains(head));
        let planted = real.replacen(head, &format!("{head}        let _ = std::fs::read(\"/etc/hostname\");\n"), 1);
        assert_eq!(run(&[(HANDLER, &planted)], None).len(), 1);
    }

    #[test]
    fn port_calls_are_judged_in_loop_modules_and_not_in_handlers() {
        let calls = "fn go(engine: &mut E) {\n    engine.pump();\n    engine.tick();\n    engine.submit(a);\n    engine.complete(k, o);\n    let e = Engine::open(p);\n    let e = open_engine(s);\n    e.with_marker(m);\n    Settings::load();\n    Executor::submit(&mut e, a);\n}\n";
        assert_eq!(run(&[("spira-runtime/src/notify.rs", calls)], None).len(), 9);
        assert!(run(&[("runtime/src/heartbeat.rs", calls)], None).is_empty());
    }

    #[test]
    fn the_known_main_offenders_are_flagged_at_their_lines() {
        let host = "async fn serve_with(mut engine: Engine) {\n    engine.run_until_idle()?;\n    tokio::task::spawn_blocking(move || r.run());\n    engine.submit(a)?;\n    engine.complete(k, o)?;\n}\n";
        let main = "use std::process;\nasync fn serve() {\n    let s = Settings::load()?;\n    let e = open_engine(&s)?;\n}\n";
        let notify = "fn send(&self) {\n    let _ = self.sock.send_to_addr(b, d);\n    let l = self.last.lock().unwrap();\n}\n";
        let got = run(&[("runtime/src/host.rs", host), ("spira-runtime/src/main.rs", main), ("spira-runtime/src/notify.rs", notify)], None);
        for want in ["host.rs:2", "host.rs:3", "host.rs:4", "host.rs:5", "main.rs:3", "main.rs:4", "notify.rs:2", "notify.rs:3"] {
            assert!(got.iter().any(|g| g.contains(want)), "{want} in {got:?}");
        }
        assert_eq!(got.len(), 8, "a `use` line is no call: {got:?}");
    }

    #[test]
    fn an_engine_turn_in_an_async_loop_fn_is_caught_unless_offloaded() {
        let bare = "async fn serve_with(mut engine: Engine) -> R {\n    engine.run_until_idle()?;\n    Ok(())\n}\n";
        let got = run(&[("runtime/src/host.rs", bare)], None);
        assert_eq!(got.len(), 1, "{got:?}");
        let off = "async fn serve_with(mut engine: Engine) -> R {\n    offload(\"turn\", None, move || { engine.run_until_idle() }).await?;\n    Ok(())\n}\n";
        assert!(run(&[("runtime/src/host.rs", off)], None).is_empty());
    }

    #[test]
    fn a_sync_fn_outside_the_regions_is_not_judged() {
        let src = "pub fn load() { let _ = std::fs::read(\"x\"); }\nasync fn go() { let _ = std::fs::read(\"x\"); }\nasync fn ok() { let a = async move { engine.pump() }; }\n";
        let got = run(&[("spira-runtime/src/world.rs", src)], None);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().any(|g| g.contains(":2")) && got.iter().any(|g| g.contains(":3")));
    }

    #[test]
    fn spawn_blocking_has_one_home() {
        let src = "async fn f() { tokio::task::spawn_blocking(|| 1).await; }\n";
        assert_eq!(run(&[("spira-runtime/src/world.rs", src)], None).len(), 1);
        assert!(run(&[(OFFLOAD_FILE, src)], None).is_empty());
    }

    #[test]
    fn the_marker_needs_a_reason_a_comment_and_a_non_handler() {
        let with = |m: &str| format!("fn send(sock: &S) {{\n    sock.send_to_addr(b, d); {m}\n}}\n");
        let notify = "spira-runtime/src/notify.rs";
        assert!(run(&[(notify, &with("// loop-safe: socket is O_NONBLOCK"))], None).is_empty());
        assert_eq!(run(&[(notify, &with("// loop-safe:"))], None).len(), 1);
        assert_eq!(run(&[(notify, "fn send(s: &S) {\n    s.send_to_addr(\"loop-safe: x\", d);\n}\n")], None).len(), 1);
        let above = "fn send(s: &S) {\n    // loop-safe: nonblocking\n    // more words\n    s.send_to_addr(b, d);\n}\n";
        assert!(run(&[(notify, above)], None).is_empty());
        let in_handler = "pub fn handle(s: &S) {\n    s.send_to_addr(b, d); // loop-safe: no\n    offload(|| std::fs::read(\"x\"));\n}\n";
        assert_eq!(run(&[(HANDLER, in_handler)], None).len(), 2);
    }

    #[test]
    fn strings_comments_tests_and_async_io_are_not_findings() {
        let src = "async fn f() {\n    let s = \"std::fs::read\";\n    // std::fs::read\n    let a = tokio::fs::read(\"x\").await;\n    let g = m.lock().await;\n}\n#[cfg(test)]\nmod t { async fn g() { let _ = std::fs::read(\"x\"); } }\n";
        assert!(run(&[("runtime/src/host.rs", src)], None).is_empty());
        assert!(run(&[("runtime/src/tests/x.rs", "async fn g() { let _ = std::fs::read(\"x\"); }\n")], None).is_empty());
    }

    #[test]
    fn a_lock_without_await_is_caught() {
        let got = run(&[("runtime/src/host.rs", "fn k(m: &M) { let g = m.lock().unwrap(); }\n")], None);
        assert_eq!(got.len(), 1, "{got:?}");
    }

    #[test]
    fn the_allow_list_only_shrinks() {
        let src = "async fn f() { let _ = std::fs::read(\"x\"); }\n";
        let f = "spira-runtime/src/world.rs";
        assert!(run(&[(f, src)], Some("1 spira-runtime/src/world.rs sp-x\n")).is_empty());
        let over = run(&[(f, src)], Some("2 spira-runtime/src/world.rs sp-x\n"));
        assert_eq!(over.len(), 1, "{over:?}");
        assert!(over[0].contains("lower it to 1"));
        let missing = run(&[(f, src)], Some("1 spira-runtime/src/world.rs sp-x\n1 runtime/src/gone.rs sp-x\n"));
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("does not exist"));
        assert_eq!(run(&[(f, "async fn f() {}\n")], None).len(), 0);
    }
}
