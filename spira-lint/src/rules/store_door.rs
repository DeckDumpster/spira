//! `store-door` — only the store's own door invokes dolt or bd. Everything else reads and
//! writes the store through spira-lc, so one program owns its credentials, deadlines and
//! lifecycle rules. Three forms are refused: a Rust `Command::new`/`bounded`/`bounded_for` of
//! `dolt` or `bd` — by literal, or by a name its crate binds from a program key (`SPIRA_BD`,
//! `SPIRA_DOLT_BIN`) — a shell command word that names either (`bd`, `$SPIRA_BD`, …), and a
//! MySQL connection to the dolt server.
//!
//! `STORE_DOOR_CONSIDERED="<why>"` on the offending line passes it and logs the reason.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::Regex;

use crate::lex::{rust, shell};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct StoreDoor;

const NAME: &str = "store-door";
const OVERRIDE: &str = "STORE_DOOR_CONSIDERED=";

/// Path prefixes (or exact files) allowed to invoke the store, each with why.
const DOORS: &[(&str, &str)] = &[
    ("spira-lc/", "the door: the one program that talks to the stores"),
    ("bead/", "the sanctioned bd wrapper for bead content"),
    ("spira/bead.sh", "the sanctioned bd wrapper (bead) for content"),
    ("spira/bdq.sh", "the sanctioned bd wrapper (bdq) for content"),
    ("spira-lint/", "the rule's own fixtures and detectors name the programs it refuses"),
    ("lifecycle-guard/", "detectors for direct bd calls carry them as fixtures"),
    ("testenv/", "builds and tears down the throwaway fixture databases tests run on"),
    ("install/", "installs, boots and probes the dolt server itself"),
    ("spira/testdb", "throwaway fixture database on an embedded dolt"),
    ("spira/dolt-", "operates the dolt server process"),
    ("spira/build-bd.sh", "builds the bd binary"),
    ("spira/bd-pin.sh", "pins the bd binary version"),
    ("spira/test-", "legacy: suite fixtures drive the throwaway store they created; shrink-only"),
    ("release/", "legacy: stages spira-lc and its tests boot a dolt server to test it; shrink-only"),
    ("store-persist/", "legacy: the persistence probe operates dolt itself; shrink-only"),
    ("spira/lc-backup.sh", "legacy: dolt backup and restore of the stores; shrink-only"),
    ("spira/acceptance-ci.sh", "legacy: prints the bd version into the acceptance report; shrink-only"),
    ("concierge.sh", "legacy: bd ping liveness probe; shrink-only"),
    ("rule/src/main.rs", "legacy: statute reads through bd; shrink-only"),
    ("docs/spikes/", "recorded measurement reruns of bd, never run by the loop"),
    ("tsd-lifecycle-export/", "legacy: exports lifecycle rows by dolt CLI; shrink-only"),
];

fn door(path: &str) -> bool {
    DOORS.iter().any(|(p, _)| path.starts_with(p))
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

/// An expression that evaluates to a store program: a read of a program key
/// (`cfg("SPIRA_BD")?`, `self.env("SPIRA_DOLT_BIN").unwrap_or(…)`), or a path to a name already
/// bound to one (`&self.bd`, `cfg.bd_bin.clone()`).
fn is_program(expr: &str, tainted: &BTreeSet<String>) -> bool {
    static READ: OnceLock<Regex> = OnceLock::new();
    static PATH: OnceLock<Regex> = OnceLock::new();
    let e = expr.trim_start();
    let e = e.strip_prefix("match ").unwrap_or(e).trim_start_matches(['&', ' ']);
    let e = e.strip_prefix("mut ").unwrap_or(e);
    if re(&READ, r#"^[A-Za-z_][\w:.]*\s*\(\s*"(?:SPIRA_BD|SPIRA_DOLT_BIN)""#).is_match(e) {
        return true;
    }
    let path = r"^(?:[A-Za-z_]\w*\s*\.\s*)*([A-Za-z_]\w*)\s*(?:\.(?:clone|to_string|to_owned|as_str|as_ref|into)\(\)|\?)*\s*(?:$|[,;)}{])";
    re(&PATH, path).captures(e).is_some_and(|c| tainted.contains(&c[1]))
}

/// Each name a line binds, with the text it is bound to: a `let`'s pattern, a `field:`
/// initialiser, or an assignment.
fn bindings(line: &str) -> Vec<(String, &str)> {
    static BIND: OnceLock<Regex> = OnceLock::new();
    let b = line.as_bytes();
    let mut out = Vec::new();
    let pat = r"\blet\s+(?:mut\s+)?([A-Za-z_]\w*)\s*(?::[^=;]*)?=|([A-Za-z_]\w*)\s*([:=])";
    for c in re(&BIND, pat).captures_iter(line) {
        let name = match (c.get(1), c.get(2), c.get(3)) {
            (Some(n), _, _) => n,
            (None, Some(n), Some(op)) => {
                let prev = n.start().checked_sub(1).map(|i| b[i]);
                let next = b.get(op.end()).copied();
                let ok = match op.as_str() {
                    ":" => next != Some(b':') && prev != Some(b':'),
                    _ => !matches!(next, Some(b'=' | b'>')),
                };
                if !ok {
                    continue;
                }
                n
            }
            _ => continue,
        };
        if !matches!(name.as_str(), "self" | "Self" | "mut") {
            out.push((name.as_str().to_string(), &line[c.get(0).unwrap().end()..]));
        }
    }
    out
}

/// Every name a crate binds to a store program, followed to a fixed point.
fn tainted_names(texts: &[String]) -> BTreeSet<String> {
    let mut tainted = BTreeSet::new();
    loop {
        let before = tainted.len();
        for line in texts.iter().flat_map(|t| t.lines()) {
            for (name, rhs) in bindings(line) {
                // A one-letter name is a closure's or a loop's, reused for anything in a crate.
                if name.len() > 1 && !tainted.contains(&name) && (is_program(rhs, &tainted) || names_store_literally(rhs)) {
                    tainted.insert(name);
                }
            }
        }
        if tainted.len() == before {
            return tainted;
        }
    }
}

/// The first argument of the call whose `(` is at `open`: up to the `,` or `)` that closes it.
fn first_arg(text: &str, open: usize) -> &str {
    let b = text.as_bytes();
    let mut depth = 0usize;
    for (i, &c) in b.iter().enumerate().skip(open + 1) {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => return &text[open + 1..i],
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => return &text[open + 1..i],
            _ => {}
        }
    }
    &text[open + 1..]
}

fn line_at(text: &str, at: usize) -> usize {
    1 + text.as_bytes()[..at].iter().filter(|&&b| b == b'\n').count()
}

fn rust_text(src: &[u8]) -> String {
    String::from_utf8_lossy(&rust::classify(src).without_comments()).into_owned()
}

/// `"bd"`, `"/opt/x/dolt"`, or `tool("bd")`: the store's program named outright.
fn names_store_literally(expr: &str) -> bool {
    static LIT: OnceLock<Regex> = OnceLock::new();
    let e = expr.trim_start().trim_start_matches('&');
    re(&LIT, r#"^\s*(?:"(?:[^"]*/)?(?:dolt|bd)"|[\w:]*\btool\s*\(\s*"(?:dolt|bd)"\s*\))\s*(?:\.(?:to_string|to_owned|into)\(\)|\.\s*clone\(\))?\s*(?:$|[,;)}])"#).is_match(e)
}

/// The top-level elements of an array literal whose `[` has been consumed.
fn elements(rest: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (i, c) in rest.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ']' | ')' | '}' if depth == 0 => {
                out.push(&rest[start..i]);
                return out;
            }
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&rest[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&rest[start..]);
    out
}

fn rust_hits(text: &str, tainted: &BTreeSet<String>) -> Vec<(usize, &'static str)> {
    static CMD: OnceLock<Regex> = OnceLock::new();
    static SQL: OnceLock<Regex> = OnceLock::new();
    let mut out = Vec::new();
    for m in re(&CMD, r"\b(?:Command::new|bounded|bounded_for)\s*\(|\.(arg|args)\s*\(").find_iter(text) {
        let arg = first_arg(text, m.end() - 1);
        let hit = if m.as_str().starts_with(".args") {
            // A wrapper's argv: `.args([secs, &bd, "-C", db])`.
            let inner = arg.trim().trim_start_matches('&').trim_start_matches("vec!").trim();
            inner.strip_prefix('[').is_some_and(|rest| elements(rest).iter().any(|e| is_program(e, tainted)))
        } else if m.as_str().starts_with(".arg") {
            is_program(arg, tainted)
        } else {
            names_store_literally(arg) || is_program(arg, tainted)
        };
        if hit {
            out.push((line_at(text, m.start()), "invokes dolt or bd directly"));
        }
    }
    let sql = re(&SQL, r#"mysql://|\bmysql_async\b|\bsqlx::mysql\b|\buse\s+mysql\b|\bmysql::"#);
    for (i, l) in text.lines().enumerate() {
        if sql.is_match(l) {
            out.push((i + 1, "connects to the dolt server over MySQL"));
        }
    }
    out
}

/// A command word that names the store's program: `bd`/`dolt` by path, or a parameter the
/// config fills with one (`$SPIRA_BD`, `${SPIRA_DOLT_BIN:-dolt}`, …).
fn store_program(word: &str) -> bool {
    static VAR: OnceLock<Regex> = OnceLock::new();
    let base = word.rsplit('/').next().unwrap_or("");
    matches!(base, "dolt" | "bd") || re(&VAR, r"^\$(?:\{(?:SPIRA_BD|SPIRA_DOLT_BIN|DOLT|BD|BD_BIN)(?::?[-=][^}]*)?\}|(?:SPIRA_BD|SPIRA_DOLT_BIN|DOLT|BD|BD_BIN)$)").is_match(word)
}

/// The argv a wrapper (`exec`, `command`, `timeout DUR`) runs.
fn unwrapped(argv: &[shell::Word]) -> &[shell::Word] {
    let mut a = argv;
    loop {
        match a.first().map(|w| w.unquoted()).as_deref() {
            Some("exec" | "command") => a = &a[1..],
            Some("timeout") => {
                let opts = a[1..].iter().take_while(|w| w.raw.starts_with('-')).count();
                a = a.get(2 + opts..).unwrap_or(&[]);
            }
            _ => return a,
        }
    }
}

fn shell_hits(src: &[u8]) -> Vec<(usize, &'static str)> {
    static SQL: OnceLock<Regex> = OnceLock::new();
    let mut out = Vec::new();
    for cmd in shell::parse(src) {
        let (_, argv) = cmd.split_env();
        let Some(head) = unwrapped(argv).first() else { continue };
        let name = head.unquoted();
        if store_program(&name) {
            out.push((head.line, "invokes dolt or bd directly"));
        } else if name.rsplit('/').next() == Some("mysql") {
            out.push((head.line, "connects to the dolt server over MySQL"));
        }
    }
    let sql = re(&SQL, r"mysql://");
    for (i, l) in String::from_utf8_lossy(src).lines().enumerate() {
        if !l.trim_start().starts_with('#') && sql.is_match(l) {
            out.push((i + 1, "connects to the dolt server over MySQL"));
        }
    }
    out
}

/// The hits of one file as findings; overridden lines are logged and dropped.
fn scan(path: &str, src: &[u8], hits: Vec<(usize, &'static str)>) -> Vec<Finding> {
    let text = String::from_utf8_lossy(src);
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (n, why) in hits {
        let line = lines.get(n - 1).copied().unwrap_or("");
        if let Some(i) = line.find(OVERRIDE) {
            let reason = line[i + OVERRIDE.len()..].trim().trim_matches(|c| c == '"' || c == '\'');
            eprintln!("{NAME}: considered {path}:{n}: {reason}");
            continue;
        }
        out.push(Finding { rule: NAME, path: path.to_string(), line: Some(n), message: format!("{why}: {}", line.trim()) });
    }
    out
}

/// The crate a Rust file belongs to: the path before its `src/`, `tests/`, `benches/` or
/// `examples/`; names flow between a crate's files, so taint is collected per crate.
fn crate_of(path: &str) -> &str {
    ["/src/", "/tests/", "/benches/", "/examples/"].iter().filter_map(|d| path.find(d)).min().map_or_else(|| path.rsplit_once('/').map_or("", |(d, _)| d), |i| &path[..i])
}

/// A Rust file's path, source, and source with comments blanked.
type RustFile<'t> = (&'t str, &'t [u8], String);

fn is_script(e: &Entry) -> bool {
    e.path.ends_with(".sh") || e.path.ends_with(".bash")
}

impl Rule for StoreDoor {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        (e.path.ends_with(".rs") || is_script(e)) && !e.path.starts_with("target/")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let mut crates: BTreeMap<&str, Vec<RustFile>> = BTreeMap::new();
        let mut out = Vec::new();
        for e in files.iter().filter(|e| !door(&e.path)) {
            let Some(c) = tree.content(e) else { continue };
            if e.path.ends_with(".rs") {
                crates.entry(crate_of(&e.path)).or_default().push((&e.path, c, rust_text(c)));
            } else {
                out.extend(scan(&e.path, c, shell_hits(c)));
            }
        }
        for files in crates.values() {
            let texts: Vec<String> = files.iter().map(|(_, _, t)| t.clone()).collect();
            let tainted = tainted_names(&texts);
            for (path, src, text) in files {
                out.extend(scan(path, src, rust_hits(text, &tainted)));
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "Only spira-lc (and the bead/bdq content wrappers) talk to the store. Read state with \
spira-lc and write content through `spira-lc content …`; add the verb there if it is missing. \
A reasoned exception carries STORE_DOOR_CONSIDERED=\"<why>\" on the line."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(StoreDoor)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        StoreDoor.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn rust_form_is_caught() {
        let t = TempDir::new("sd-rs");
        t.write("foo/src/a.rs", "fn f() { Command::new(\"dolt\").arg(\"sql\"); }\nfn g() { Command::new(\"/opt/x/bd\"); }\n");
        assert_eq!(run(&t, &["foo/src/a.rs"]).unwrap().len(), 2);
    }

    #[test]
    fn shell_form_is_caught() {
        let t = TempDir::new("sd-sh");
        t.write("foo/x.sh", "out=$(bd -C \"$db\" list)\nDOLT_X=1 dolt sql -q 'select 1'\n\"$BD\" close x\n");
        assert_eq!(run(&t, &["foo/x.sh"]).unwrap().len(), 3);
    }

    #[test]
    fn sql_connection_form_is_caught() {
        let t = TempDir::new("sd-sql");
        t.write("foo/src/a.rs", "let u = \"mysql://root@127.0.0.1:3306/db\";\n");
        t.write("foo/y.sh", "mysql -h 127.0.0.1 -P 3306 -e 'select 1'\n");
        assert_eq!(run(&t, &["foo/src/a.rs", "foo/y.sh"]).unwrap().len(), 2);
    }

    #[test]
    fn an_allow_listed_door_passes() {
        let t = TempDir::new("sd-door");
        t.write("spira-lc/src/a.rs", "fn f() { Command::new(\"dolt\"); }\n");
        t.write("foo/src/b.rs", "fn f() {}\n");
        assert_eq!(run(&t, &["spira-lc/src/a.rs", "foo/src/b.rs"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn override_and_comments_pass() {
        let t = TempDir::new("sd-ok");
        t.write("foo/a.rs", "// Command::new(\"bd\")\nfn f() { Command::new(\"bd\"); } // STORE_DOOR_CONSIDERED=\"probe\"\nlet s = \"bd: refused\";\n");
        t.write("foo/x.sh", "# bd list\necho bd\nspira-lc state x\n");
        assert_eq!(run(&t, &["foo/a.rs", "foo/x.sh"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_program_read_from_config_is_caught() {
        let t = TempDir::new("sd-cfg");
        t.write("foo/src/a.rs", "fn f() { let bd = cfg(\"SPIRA_BD\")?; Command::new(&bd); }\n");
        t.write("foo/src/b.rs", "fn g(&self) { bounded(self.env(\"SPIRA_DOLT_BIN\").unwrap_or_else(|| \"x\".into())).arg(\"sql\"); }\n");
        t.write("foo/x.sh", "\"$SPIRA_BD\" list\n${SPIRA_DOLT_BIN:-dolt} sql\ntimeout 5 \"${SPIRA_BD}\" show x\n");
        let got = run(&t, &["foo/src/a.rs", "foo/src/b.rs", "foo/x.sh"]).unwrap();
        assert_eq!(got.len(), 5, "{got:?}");
    }

    #[test]
    fn a_field_bound_from_config_in_another_file_of_the_crate_is_caught() {
        let t = TempDir::new("sd-field");
        t.write("foo/src/cfg.rs", "fn load() -> Config { Config {\n    db: cfg(\"SPIRA_DB\")?,\n    bd: cfg(\"SPIRA_BD\")?,\n} }\n");
        t.write("foo/src/store.rs", "fn new(c: &Config) -> Store { Store { bin: c.bd.clone() } }\nfn cmd(&self) -> Command {\n    let mut c = Command::new(\n        &self.bin,\n    );\n    c\n}\n");
        t.write("bar/src/main.rs", "fn f(s: &S) { Command::new(&s.bin); }\n");
        let got = run(&t, &["foo/src/cfg.rs", "foo/src/store.rs", "bar/src/main.rs"]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("foo/src/store.rs:3"), "{got:?}");
    }

    #[test]
    fn a_store_program_handed_to_a_wrapper_is_caught() {
        let t = TempDir::new("sd-wrap");
        t.write("foo/src/a.rs", "struct R { bd_bin: String }\nfn new() -> R { R { bd_bin: cfg(\"SPIRA_BD\")? } }\nfn f(r: &R) { Command::new(\"timeout\").arg(\"5\").arg(&r.bd_bin).arg(\"list\"); }\n");
        let got = run(&t, &["foo/src/a.rs"]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("foo/src/a.rs:3"), "{got:?}");
    }

    #[test]
    fn other_programs_and_wrapper_methods_pass() {
        let t = TempDir::new("sd-neg");
        t.write("foo/src/a.rs", "fn f(&self) { let git = cfg(\"SPIRA_GIT\")?; Command::new(&git); let bd = cfg(\"SPIRA_BD\")?; self.bd(&[\"x\"]); Command::new(\"spira-lc\").env(\"X\", &bd); }\n");
        t.write("foo/x.sh", "\"$SPIRA_LC\" list\necho \"$SPIRA_BD\"\n");
        assert_eq!(run(&t, &["foo/src/a.rs", "foo/x.sh"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("sd-none");
        t.write("README.md", "bd\n");
        assert!(run(&t, &["README.md"]).is_err());
    }
}
