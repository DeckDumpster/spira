//! `bdq-read` — a file outside the store's own code gains no `bdq` read. Reads of bead content
//! go through `spira-lc content <verb>`. Reads that predate the rule are counted per file in
//! `spira-lint/bdq-read-allow`, which only shrinks. Writes are not counted.
//!
//! `BDQ_READ_CONSIDERED="<why>"` on the line passes it and logs `bdq-read: considered <path>:<n>: <why>`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;

use crate::lex::shell;
use crate::rules::call_deadline::parse_count_allow;
use crate::rules::store_door::{crate_of, line_at, rust_text, unwrapped};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct BdqRead;

const NAME: &str = "bdq-read";
const ALLOW_FILE: &str = "spira-lint/bdq-read-allow";
const OVERRIDE: &str = "BDQ_READ_CONSIDERED=";

/// Path prefixes (or exact files) that may read through bdq, each with why.
const EXEMPT: &[(&str, &str)] = &[
    ("spira-lc/", "the door: its content verbs are what bdq reads are moved to"),
    ("bead/src/bdq.rs", "the argv classifiers the write guards share"),
    ("spira/bdq.sh", "the bdq wrapper itself"),
    ("spira-lint/", "the rule's own fixtures and detectors name the reads it refuses"),
    ("spira/test-", "suite fixtures drive the throwaway store they created"),
];

const READ_VERBS: &[&str] = &[
    "show", "list", "search", "state", "memories", "comments", "children", "ready", "blocked", "history", "sql", "count", "query",
    "stats", "status", "graph", "find", "export",
];

/// Verbs that read only with a given second word: `dep list`, `label list`, `kv get`, `config get`.
const COMPOUND: &[(&str, &str)] = &[("dep", "list"), ("label", "list"), ("kv", "get"), ("config", "get")];

/// Lifecycle-client calls whose argv is the door's, not bd's.
const CLIENT_CALLS: &[&str] = &[".call(", ".answer(", ".call_stdin(", "lc_run(", "lc_full(", "run_lc(", "lc_list(", "call_held(", "held_read(", "lc_call("];

type CrateFile<'t> = (&'t Entry, &'t [u8], String);

fn exempt(path: &str) -> bool {
    EXEMPT.iter().any(|(p, _)| path.starts_with(p)) || path.starts_with("tests/") || path.contains("/tests/")
}

fn is_read(first: &str, second: Option<&str>) -> bool {
    READ_VERBS.contains(&first) || COMPOUND.iter().any(|(a, b)| *a == first && Some(*b) == second)
}

fn is_script(e: &Entry) -> bool {
    e.path.ends_with(".sh") || e.path.ends_with(".bash")
}

fn shell_hits(src: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    for cmd in shell::parse(src) {
        let (_, argv) = cmd.split_env();
        let argv = unwrapped(argv);
        let Some(head) = argv.first() else { continue };
        let name = head.unquoted();
        if !matches!(name.rsplit('/').next(), Some("bdq" | "bdjson")) {
            continue;
        }
        let Some(verb) = argv.get(1).map(|w| w.unquoted()) else { continue };
        let second = argv.get(2).map(|w| w.unquoted());
        if is_read(&verb, second.as_deref()) {
            out.push(head.line);
        }
    }
    out
}

/// Whether the text before `at` sits inside the parentheses of a lifecycle-client call.
fn in_client_call(text: &str, at: usize) -> bool {
    let b = text.as_bytes();
    let mut depth = 0usize;
    for i in (0..at).rev() {
        match b[i] {
            b')' => depth += 1,
            b'(' if depth > 0 => depth -= 1,
            b'(' => {
                let head = &text[..=i];
                if CLIENT_CALLS.iter().any(|c| head.ends_with(c)) {
                    return true;
                }
            }
            b';' | b'{' | b'}' if depth == 0 => return false,
            _ => {}
        }
    }
    false
}

fn rust_hits(text: &str) -> Vec<usize> {
    static ARGV: OnceLock<Regex> = OnceLock::new();
    let re = ARGV.get_or_init(|| Regex::new(r#"(?:&\s*\[|\bvec!\s*\[)\s*"([a-z-]+)"(?:\s*,\s*"([a-z-]+)")?"#).expect("static regex"));
    re.captures_iter(text)
        .filter(|c| is_read(&c[1], c.get(2).map(|m| m.as_str())))
        .filter(|c| !in_client_call(text, c.get(0).unwrap().start()))
        .map(|c| line_at(text, c.get(0).unwrap().start()))
        .collect()
}

/// Every unexcused read in the walk: path and line, sorted. Overridden lines are logged and dropped.
fn violations(tree: &Tree) -> Result<Vec<(String, usize)>, LintError> {
    let files = crate::scope(tree, &BdqRead)?;
    let mut crates: BTreeMap<&str, Vec<CrateFile>> = BTreeMap::new();
    let mut out = Vec::new();
    let keep = |path: &str, src: &[u8], lines: Vec<usize>, out: &mut Vec<(String, usize)>| {
        let text = String::from_utf8_lossy(src);
        let all: Vec<&str> = text.lines().collect();
        for n in lines {
            let line = all.get(n - 1).copied().unwrap_or("");
            if let Some(i) = line.find(OVERRIDE) {
                let reason = line[i + OVERRIDE.len()..].trim().trim_matches(|c| c == '"' || c == '\'');
                eprintln!("{NAME}: considered {path}:{n}: {reason}");
                continue;
            }
            out.push((path.to_string(), n));
        }
    };
    for e in &files {
        let Some(c) = tree.content(e) else { continue };
        if e.path.ends_with(".rs") {
            crates.entry(crate_of(&e.path)).or_default().push((e, c, rust_text(c)));
        } else if !exempt(&e.path) {
            keep(&e.path, c, shell_hits(c), &mut out);
        }
    }
    for fs in crates.values() {
        if !fs.iter().any(|(_, _, t)| t.contains("\"bdq\"")) {
            continue;
        }
        for (e, c, t) in fs.iter().filter(|(e, _, _)| !exempt(&e.path)) {
            keep(&e.path, c, rust_hits(t), &mut out);
        }
    }
    out.sort();
    Ok(out)
}

/// The allow file's text for the tree as it stands: what `--emit-allow` prints.
pub fn render_allow(tree: &Tree) -> Result<String, LintError> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (p, _) in violations(tree)? {
        *counts.entry(p).or_default() += 1;
    }
    let mut s = String::from(
        "# bdq-read: files that read through bdq, as `<count> <path>`. Generated:\n\
# spira-lint --only bdq-read --emit-allow.\n\
#\n\
# SHRINK-ONLY. A count goes down as reads move to `spira-lc content <verb>`; nothing is\n\
# raised for a NEW read.\n",
    );
    for (p, n) in counts {
        s.push_str(&format!("{n} {p}\n"));
    }
    Ok(s)
}

impl Rule for BdqRead {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        (e.path.ends_with(".rs") || is_script(e)) && !e.path.starts_with("target/")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let allow = parse_count_allow(ALLOW_FILE, &tree.read_text(ALLOW_FILE))?;
        let mut by_path: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (p, n) in violations(tree)? {
            by_path.entry(p).or_default().push(n);
        }
        let mut out = Vec::new();
        for (path, lines) in &by_path {
            let allowed = allow.get(path).map_or(0, |a| a.0);
            if lines.len() <= allowed {
                continue;
            }
            let note = if allowed == 0 { String::new() } else { format!(" ({} allowed, {} found)", allowed, lines.len()) };
            for n in lines {
                out.push(Finding { rule: NAME, path: path.clone(), line: Some(*n), message: format!("bdq read outside the store{note}") });
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
        "Read bead content through `spira-lc content <verb>` (show, list, state, memories, comments, search, \
dep list, label list, events); add the verb to the door if it is missing (`spira-lc/src/content.rs::check`). \
Writes are not counted. A reasoned exception carries BDQ_READ_CONSIDERED=\"<why>\" on the line. \
Existing reads are counted in spira-lint/bdq-read-allow, which only shrinks; regenerate with \
`spira-lint --only bdq-read --emit-allow`."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(BdqRead)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const RUST: &str = "fn f() { let _ = tool(\"bdq\"); }\n";

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        BdqRead.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    fn hits(t: &TempDir, files: &[&str]) -> usize {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        violations(&tree).unwrap().len()
    }

    #[test_id(t389t78)]
    #[test]
    fn a_read_asked_of_the_held_view_or_the_socket_is_the_doors_not_bdqs() {
        let t = TempDir::new("br-held");
        t.write("a/src/x.rs", &format!("{RUST}fn g() {{ call_held(&c, &[\"list\", \"--live\"]); held_read(&[\"list\"]); lc_call(&p, &[\"list\"]); }}\n"));
        t.write("b/src/y.rs", &format!("{RUST}fn g() {{ other(&[\"list\", \"--live\"]); }}\n"));
        assert_eq!(hits(&t, &["a/src/x.rs"]), 0);
        assert_eq!(hits(&t, &["b/src/y.rs"]), 1, "the matcher still finds a bare read");
    }

    #[test_id(td383la)]
    #[test]
    fn shell_reads_are_hits_and_writes_are_not() {
        let t = TempDir::new("br-sh");
        t.write("spira/x.sh", "bdq show \"$id\" --json\nbdjson list --all\nbdq note \"$id\" x\nbdq label add a b\nbdq dep list a\nbdq dep add a b\ntimeout 5 bdq state a\n");
        assert_eq!(hits(&t, &["spira/x.sh"]), 4);
    }

    #[test_id(tx5hejl)]
    #[test]
    fn a_shell_comment_and_a_suite_are_not_hits() {
        let t = TempDir::new("br-shc");
        t.write("spira/x.sh", "# bdq show x\nspira-lc content show x\n");
        t.write("spira/test-x.sh", "bdq show x\n");
        assert_eq!(hits(&t, &["spira/x.sh", "spira/test-x.sh"]), 0);
    }

    #[test_id(tfkejux)]
    #[test]
    fn rust_argv_in_a_bdq_crate_is_a_hit() {
        let t = TempDir::new("br-rs");
        t.write("c/src/a.rs", &format!("{RUST}fn g() {{\n    run(&[\"show\", id, \"--json\"]);\n    bdq_capture(home, &s(&[\"dep\", \"list\", id]));\n    run(&[\"note\", id]);\n    run(&[\"dep\", \"add\", a, b]);\n}}\n"));
        assert_eq!(hits(&t, &["c/src/a.rs"]), 2);
    }

    #[test_id(t0q4f6h)]
    #[test]
    fn door_calls_and_crates_without_bdq_are_not_hits() {
        let t = TempDir::new("br-door");
        t.write("c/src/a.rs", &format!("{RUST}fn g() {{ lc.call(&[\"show\", id]); door(&[\"content\", \"show\", id]); lc_run(&s(&[\"list\"])); }}\n"));
        t.write("d/src/a.rs", "fn g() { run(&[\"show\", id]); }\n");
        assert_eq!(hits(&t, &["c/src/a.rs", "d/src/a.rs"]), 0);
    }

    #[test_id(txiphxl)]
    #[test]
    fn a_crates_tests_dir_is_exempt() {
        let t = TempDir::new("br-tests");
        t.write("c/src/a.rs", RUST);
        t.write("c/tests/b.rs", "fn g() { run(&[\"show\", id]); }\n");
        assert_eq!(hits(&t, &["c/src/a.rs", "c/tests/b.rs"]), 0);
    }

    #[test_id(tj8jnsw)]
    #[test]
    fn a_comment_naming_the_read_is_not_a_hit_with_or_without_the_door_call() {
        let t = TempDir::new("br-mut");
        t.write("c/src/a.rs", &format!("{RUST}// was: bdq show, run(&[\"show\", id])\nfn g() {{ door(&[\"content\", \"show\"]); }}\n"));
        assert_eq!(hits(&t, &["c/src/a.rs"]), 0);
        t.write("c/src/a.rs", &format!("{RUST}// run(&[\"show\", id]) bdq show\n"));
        assert_eq!(hits(&t, &["c/src/a.rs"]), 0);
        t.write("spira/x.sh", "# bdq show x\nspira-lc content show x\n");
        assert_eq!(hits(&t, &["spira/x.sh"]), 0);
        t.write("spira/x.sh", "# bdq show x\n");
        assert_eq!(hits(&t, &["spira/x.sh"]), 0);
    }

    #[test_id(tnt2bqa)]
    #[test]
    fn the_allow_list_counts_and_only_shrinks() {
        let t = TempDir::new("br-allow");
        t.write("spira/x.sh", "bdq show a\nbdq list\n");
        t.write(ALLOW_FILE, "2 spira/x.sh\n");
        assert_eq!(run(&t, &["spira/x.sh"]).unwrap(), Vec::<String>::new());
        t.write("spira/x.sh", "bdq show a\nbdq list\nbdq state a\n");
        let got = run(&t, &["spira/x.sh"]).unwrap();
        assert_eq!(got.len(), 3);
        assert!(got[0].ends_with("(2 allowed, 3 found)"), "{got:?}");
        t.write("spira/x.sh", "bdq show a\n");
        assert_eq!(
            run(&t, &["spira/x.sh"]).unwrap(),
            vec!["bdq-read: spira-lint/bdq-read-allow:1: allows 2 for spira/x.sh, which now has 1 — lower it to 1 (the list only shrinks)"]
        );
        t.write(ALLOW_FILE, "");
        assert_eq!(run(&t, &["spira/x.sh"]).unwrap(), vec!["bdq-read: spira/x.sh:1: bdq read outside the store"]);
    }

    #[test_id(tebn4p1)]
    #[test]
    fn a_considered_line_passes() {
        let t = TempDir::new("br-considered");
        t.write("spira/x.sh", "bdq show a # BDQ_READ_CONSIDERED=\"probe\"\n");
        t.write(ALLOW_FILE, "");
        assert_eq!(run(&t, &["spira/x.sh"]).unwrap(), Vec::<String>::new());
    }

    #[test_id(t2nrs1u)]
    #[test]
    fn render_allow_round_trips() {
        let t = TempDir::new("br-render");
        t.write("spira/x.sh", "bdq show a\nbdq list\n");
        let tree = Tree::from_paths(t.path(), ["spira/x.sh"], std::iter::empty::<&str>());
        let text = render_allow(&tree).unwrap();
        assert!(text.contains("\n2 spira/x.sh\n"), "{text}");
        t.write(ALLOW_FILE, &text);
        assert_eq!(run(&t, &["spira/x.sh"]).unwrap(), Vec::<String>::new());
    }

    #[test_id(tkle53y)]
    #[test]
    fn the_shipped_list_counts_the_known_forks() {
        let list = parse_count_allow(ALLOW_FILE, include_str!("../../bdq-read-allow")).unwrap();
        for p in ["dwarf/src/run.rs", "bead/src/main.rs"] {
            assert!(list.contains_key(p), "{p} missing from the allow list");
        }
    }
}
