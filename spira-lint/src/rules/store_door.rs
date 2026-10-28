//! `store-door` — only the store's own door invokes dolt or bd. Everything else reads and
//! writes the store through spira-lc, so one program owns its credentials, deadlines and
//! lifecycle rules. Three forms are refused: a Rust `Command::new` of `dolt` or `bd`, a shell
//! command word `dolt` / `bd`, and a MySQL connection to the dolt server.
//!
//! `STORE_DOOR_CONSIDERED="<why>"` on the offending line passes it and logs the reason.

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
    ("runtime/src/store.rs", "legacy: DoltCli store adapter; shrink-only, leaves with its replacement by spira-lc"),
    ("runtime/tests/common/mod.rs", "the container tier's dolt sql fixture; production reaches the store through spira-lc"),
];

fn door(path: &str) -> bool {
    DOORS.iter().any(|(p, _)| path.starts_with(p))
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

fn rust_hits(src: &[u8]) -> Vec<(usize, &'static str)> {
    static CMD: OnceLock<Regex> = OnceLock::new();
    static SQL: OnceLock<Regex> = OnceLock::new();
    let c = rust::classify(src);
    let text = String::from_utf8_lossy(&c.without_comments()).into_owned();
    let mut out = Vec::new();
    let cmd = re(&CMD, r#"(?:Command::new|bounded|bounded_for)\s*\(\s*"(?:[^"]*/)?(?:dolt|bd)""#);
    let sql = re(&SQL, r#"mysql://|\bmysql_async\b|\bsqlx::mysql\b|\buse\s+mysql\b|\bmysql::"#);
    for (i, l) in text.lines().enumerate() {
        if cmd.is_match(l) {
            out.push((i + 1, "invokes dolt or bd directly"));
        }
        if sql.is_match(l) {
            out.push((i + 1, "connects to the dolt server over MySQL"));
        }
    }
    out
}

fn shell_hits(src: &[u8]) -> Vec<(usize, &'static str)> {
    static SQL: OnceLock<Regex> = OnceLock::new();
    let mut out = Vec::new();
    for cmd in shell::parse(src) {
        let (_, argv) = cmd.split_env();
        let Some(head) = argv.first() else { continue };
        let name = head.unquoted();
        let base = name.rsplit('/').next().unwrap_or("");
        if matches!(base, "dolt" | "bd") || matches!(name.as_str(), "$DOLT" | "${DOLT}" | "$BD" | "${BD}" | "$BD_BIN" | "${BD_BIN}") {
            out.push((head.line, "invokes dolt or bd directly"));
        } else if base == "mysql" {
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
fn scan(path: &str, src: &[u8]) -> Vec<Finding> {
    let hits = if path.ends_with(".rs") { rust_hits(src) } else { shell_hits(src) };
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
        let mut out = Vec::new();
        for e in files.iter().filter(|e| !door(&e.path)) {
            if let Some(c) = tree.content(e) {
                out.extend(scan(&e.path, c));
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
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("sd-none");
        t.write("README.md", "bd\n");
        assert!(run(&t, &["README.md"]).is_err());
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(StoreDoor)]
}
