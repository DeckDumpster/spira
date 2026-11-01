//! `two-plane` — the control plane holds no planning data, and calls a lifecycle record a
//! lifecycle record, never a bead. The planes are joined only by bead id. Contract: DESIGN.md.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

use crate::lex::rust::{self, Class};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct TwoPlane;

const NAME: &str = "two-plane";
const ALLOW_FILE: &str = "spira-lint/two-plane-allow";
const RECORDS_FILE: &str = "spira-lint/two-plane-records";

/// The planning plane's fields: bd owns them, and the control plane reads them through the
/// planning verbs and never copies them.
const PLANNING: &[&str] = &["title", "body", "description", "labels", "priority", "issue_type", "comments", "depends_on", "dep_type", "parent"];

const VOCABULARY: &str = "a lifecycle record is not a bead: name it lifecycle/<id> / LifecycleRecord";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

fn planning(name: &str) -> bool {
    PLANNING.contains(&name)
}

/// A dependency or parent edge table: the graph is planning data whatever it is called.
fn edge_table(name: &str) -> bool {
    name.ends_with("_dep") || name.ends_with("_edge")
}

fn control_rust(path: &str) -> bool {
    path.ends_with(".rs")
        && (path.starts_with("lifecycle/") || path.starts_with("spira-lc/src/") || path.split('/').next().is_some_and(|d| d.starts_with("state-store")))
}

fn control_sql(path: &str) -> bool {
    path == "lifecycle/schema.sql" || (path.starts_with("lifecycle/migrations/") && path.ends_with(".sql"))
}

/// One finding before the allow list: `what` is the allow-list key after `<path>:`.
struct Hit {
    line: usize,
    what: String,
    message: String,
}

fn hit(line: usize, what: impl Into<String>, message: impl Into<String>) -> Hit {
    Hit { line, what: what.into(), message: message.into() }
}

/// SQL with `--` comments blanked, newlines kept.
fn sql_code(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("--") {
            Some(i) => format!("{}{}", &l[..i], " ".repeat(l.len() - i)),
            None => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn line_at(text: &str, at: usize) -> usize {
    1 + text[..at].bytes().filter(|&b| b == b'\n').count()
}

/// The top-level elements of a parenthesised column list starting just after its `(`, each
/// with its byte offset.
fn elements(text: &str, open: usize) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0usize, open);
    for (i, c) in text[open..].char_indices().map(|(i, c)| (open + i, c)) {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => {
                out.push((start, &text[start..i]));
                return out;
            }
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push((start, &text[start..i]));
                start = i + 1;
            }
            _ => {}
        }
    }
    out
}

fn table_hit(line: usize, table: &str) -> Option<Hit> {
    let why = if edge_table(table) {
        "is an edge table: the dependency and parent graph belongs to bd"
    } else if planning(table) {
        "is named for planning data, which belongs to bd"
    } else {
        return None;
    };
    Some(hit(line, table, format!("table {table} {why}")))
}

fn column_hit(line: usize, table: &str, name: &str) -> Option<Hit> {
    planning(name).then(|| hit(line, format!("{table}.{name}"), format!("column {table}.{name} copies planning data")))
}

fn sql_hits(src: &str) -> Vec<Hit> {
    static CREATE: OnceLock<Regex> = OnceLock::new();
    static ALTER: OnceLock<Regex> = OnceLock::new();
    static CLAUSE: OnceLock<Regex> = OnceLock::new();
    static COL: OnceLock<Regex> = OnceLock::new();
    let text = sql_code(src);
    let name = r"`?(?:\w+`?\.`?)?(\w+)`?";
    let create = re(&CREATE, &format!(r"(?i)\bCREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?{name}\s*\("));
    let alter = re(&ALTER, &format!(r"(?i)\bALTER\s+TABLE\s+{name}([^;]*)"));
    let clause = re(
        &CLAUSE,
        r"(?i)\bADD\s+(?:COLUMN\s+)?(?:IF\s+NOT\s+EXISTS\s+)?\(?\s*`?(\w+)|\bCHANGE\s+(?:COLUMN\s+)?`?\w+`?\s+`?(\w+)|\bRENAME\s+COLUMN\s+`?\w+`?\s+TO\s+`?(\w+)|\bRENAME\s+(?:TO|AS)\s+`?(?:\w+`?\.`?)?(\w+)",
    );
    let col = re(&COL, r"^\s*`?(\w+)`?");
    let mut out = Vec::new();
    for c in create.captures_iter(&text) {
        let (m, table) = (c.get(1).expect("group"), c[1].to_ascii_lowercase());
        if let Some(h) = table_hit(line_at(&text, m.start()), &table) {
            out.push(h);
            continue;
        }
        for (at, el) in elements(&text, c.get(0).expect("whole").end()) {
            if let Some(cm) = col.captures(el) {
                let g = cm.get(1).expect("group");
                out.extend(column_hit(line_at(&text, at + g.start()), &table, &g.as_str().to_ascii_lowercase()));
            }
        }
    }
    for c in alter.captures_iter(&text) {
        let table = c[1].to_ascii_lowercase();
        let body = c.get(2).expect("group");
        for k in clause.captures_iter(body.as_str()) {
            let (i, g) = (1..=4).find_map(|i| k.get(i).map(|g| (i, g))).expect("one alternative matched");
            let (line, n) = (line_at(&text, body.start() + g.start()), g.as_str().to_ascii_lowercase());
            out.extend(if i == 4 { table_hit(line, &n) } else { column_hit(line, &table, &n) });
        }
    }
    out
}

/// The source with everything but string literals blanked, newlines kept.
fn literals_only(c: &rust::Classified) -> String {
    let masked: Vec<u8> = c.src.iter().zip(&c.class).map(|(&b, &k)| if k == Class::Literal || b == b'\n' { b } else { b' ' }).collect();
    String::from_utf8_lossy(&masked).into_owned()
}

/// Byte ranges of literal runs that read as one SQL text: a literal, however many lines it
/// spans, joined to the next when only whitespace, commas and brackets separate them (a column
/// array, a concatenation across lines).
fn literal_groups(c: &rust::Classified) -> Vec<(usize, usize)> {
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < c.class.len() {
        if c.class[i] != Class::Literal {
            i += 1;
            continue;
        }
        let start = i;
        while i < c.class.len() && c.class[i] == Class::Literal {
            i += 1;
        }
        let joins = groups.last().is_some_and(|&(_, end)| c.src[end..start].iter().all(|b| b.is_ascii_whitespace() || b",[]".contains(b)));
        match groups.last_mut() {
            Some(g) if joins => g.1 = i,
            _ => groups.push((start, i)),
        }
    }
    groups
}

/// SQL held in Rust literals: an edge table named anywhere, and a planning column in a
/// literal group, or on a line, whose literals also address the control plane's record
/// (`bead_id`, or the `bead` table).
fn rust_sql_hits(c: &rust::Classified) -> Vec<Hit> {
    static WORD: OnceLock<Regex> = OnceLock::new();
    static TABLE: OnceLock<Regex> = OnceLock::new();
    let word = re(&WORD, r"[A-Za-z0-9_]+");
    let table = re(&TABLE, r"(?i)\b(?:FROM|JOIN|INTO|UPDATE)\s+(?:spira_lifecycle\.)?bead\b");
    let addresses = |s: &str| table.is_match(s) || word.find_iter(s).any(|m| m.as_str() == "bead_id");
    let text = literals_only(c);
    let record_lines: BTreeSet<usize> = text.lines().enumerate().filter(|(_, l)| addresses(l)).map(|(i, _)| i + 1).collect();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (start, end) in literal_groups(c) {
        let group = &text[start..end];
        let record = addresses(group);
        for m in word.find_iter(group) {
            let (w, line) = (m.as_str(), c.line_of(start + m.start()));
            let found = if edge_table(w) && w.starts_with(|ch: char| ch.is_ascii_lowercase()) {
                format!("reads or writes edge table {w}: the dependency graph belongs to bd")
            } else if planning(w) && (record || record_lines.contains(&line)) {
                format!("carries planning field {w} on a lifecycle record: read it through the planning verbs")
            } else {
                continue;
            };
            if seen.insert((line, w)) {
                out.push(hit(line, w, found));
            }
        }
    }
    out
}

fn vocabulary_hits(c: &rust::Classified) -> Vec<Hit> {
    static KEY: OnceLock<Regex> = OnceLock::new();
    static TYPE: OnceLock<Regex> = OnceLock::new();
    let key = re(&KEY, r#""(beads?/)"#);
    let ty = re(&TYPE, r"\b(?:struct|enum|union|trait|type)\s+(Bead[A-Za-z0-9_]*?(?:Row|Record))\b");
    let text = String::from_utf8_lossy(&c.without_comments()).into_owned();
    let code = String::from_utf8_lossy(&c.code_only()).into_owned();
    let mut out = Vec::new();
    for m in key.captures_iter(&text) {
        let at = m.get(0).expect("whole").start();
        if c.class.get(at) == Some(&Class::Literal) {
            let prefix = &m[1];
            out.push(hit(c.line_of(at), format!("\"{prefix}\""), format!("key literal \"{prefix}…\": {VOCABULARY}")));
        }
    }
    for m in ty.captures_iter(&code) {
        let at = m.get(1).expect("group").start();
        out.push(hit(c.line_of(at), &m[1], format!("type {}: {VOCABULARY}", &m[1])));
    }
    out
}

/// The named fields of `struct <ty> { … }` in `src`, each with its line; `None` when the
/// file defines no such braced struct.
fn struct_fields(src: &[u8], ty: &str) -> Option<Vec<(usize, String)>> {
    static FIELD: OnceLock<Regex> = OnceLock::new();
    let field = re(&FIELD, r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:r#)?([a-z_][a-z0-9_]*)\s*:");
    let c = rust::classify(src);
    let code = String::from_utf8_lossy(&c.code_only()).into_owned();
    let head = Regex::new(&format!(r"\bstruct\s+{}\b[^{{;(]*\{{", regex::escape(ty))).expect("escaped regex");
    let open = head.find(&code)?.end();
    let mut depth = 0usize;
    let mut out = Vec::new();
    let mut line_start = open;
    for (i, ch) in code[open..].char_indices().map(|(i, ch)| (open + i, ch)) {
        match ch {
            '{' | '(' | '[' | '<' => depth += 1,
            '}' if depth == 0 => {
                scan_field(&code[line_start..i], field, &c, line_start, &mut out);
                return Some(out);
            }
            '}' | ')' | ']' | '>' => depth = depth.saturating_sub(1),
            ',' | '\n' if depth == 0 => {
                scan_field(&code[line_start..i], field, &c, line_start, &mut out);
                line_start = i + 1;
            }
            _ => {}
        }
    }
    None
}

fn scan_field(seg: &str, field: &Regex, c: &rust::Classified, at: usize, out: &mut Vec<(usize, String)>) {
    static ATTRS: OnceLock<Regex> = OnceLock::new();
    let skip = re(&ATTRS, r"^\s*(?:#\s*\[[^\]]*\]\s*)*").find(seg).map_or(0, |m| m.end());
    if let Some(m) = field.captures(&seg[skip..]) {
        let g = m.get(1).expect("group");
        out.push((c.line_of(at + skip + g.start()), g.as_str().to_string()));
    }
}

/// `<path>::<Type>` lines of the record registry, each with its line number.
fn records(tree: &Tree) -> Result<Vec<(usize, String, String)>, LintError> {
    let text = tree.read_text(RECORDS_FILE);
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((path, ty)) = t.split_once("::").filter(|(p, ty)| !p.is_empty() && !ty.is_empty() && !ty.contains(char::is_whitespace)) else {
            return Err(LintError::BadAllow { file: RECORDS_FILE.into(), line: i + 1, reason: format!("{t:?} is not <path>::<Type>") });
        };
        out.push((i + 1, path.to_string(), ty.to_string()));
    }
    if out.is_empty() {
        return Err(LintError::Refused(format!("{RECORDS_FILE}: no control-plane record type is registered — refusing to report clean")));
    }
    Ok(out)
}

fn record_hits(tree: &Tree) -> Result<Vec<(String, Hit)>, LintError> {
    let mut out = Vec::new();
    for (n, path, ty) in records(tree)? {
        let missing = || LintError::Refused(format!("{RECORDS_FILE}:{n}: {path}::{ty} — no such braced struct in the tree; fix or remove the line"));
        let src = tree.entry(&path).and_then(|e| tree.content(e)).ok_or_else(missing)?;
        for (line, f) in struct_fields(src, &ty).ok_or_else(missing)? {
            if planning(&f) {
                out.push((path.clone(), hit(line, format!("{ty}.{f}"), format!("record {ty} carries planning field {f}"))));
            }
        }
    }
    Ok(out)
}

/// The allow list: `<path>:<what> <removed-by>` per line, `<removed-by>` being the work item
/// that removes the entry.
struct Allow(Vec<(usize, String, String)>);

impl Allow {
    fn parse(text: &str) -> Result<Allow, LintError> {
        let mut out = Vec::new();
        for (i, l) in text.lines().enumerate() {
            let n = i + 1;
            let t = l.trim_start();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            let mut it = l.split_whitespace();
            let by = (it.next(), it.next(), it.next());
            let (Some(entry), Some(_)) = (by.0, by.1.filter(|b| b.strip_prefix("sp-").is_some_and(|id| !id.is_empty()) ||(*b == "kept" && by.2.is_some()))) else {
                return Err(LintError::BadAllow {
                    file: ALLOW_FILE.into(),
                    line: n,
                    reason: format!("{l:?} is not `<path>:<what> <work-item-id>` or `<path>:<what> kept <why>`"),
                });
            };
            let Some((path, what)) = entry.split_once(':').filter(|(p, w)| !p.is_empty() && !w.is_empty()) else {
                return Err(LintError::BadAllow { file: ALLOW_FILE.into(), line: n, reason: format!("{entry:?} is not `<path>:<what>`") });
            };
            out.push((n, path.to_string(), what.to_string()));
        }
        Ok(Allow(out))
    }
}

impl Rule for TwoPlane {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        control_sql(&e.path) || control_rust(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let allow = Allow::parse(&tree.read_text(ALLOW_FILE))?;
        let mut hits: Vec<(String, Hit)> = record_hits(tree)?;
        for e in files {
            let Some(src) = tree.content(e) else { continue };
            if control_sql(&e.path) {
                hits.extend(sql_hits(&String::from_utf8_lossy(src)).into_iter().map(|h| (e.path.clone(), h)));
            } else {
                let c = rust::classify(src);
                hits.extend(rust_sql_hits(&c).into_iter().chain(vocabulary_hits(&c)).map(|h| (e.path.clone(), h)));
            }
        }
        let mut used = BTreeSet::new();
        let mut out = Vec::new();
        for (path, h) in hits {
            match allow.0.iter().find(|(_, p, w)| *p == path && *w == h.what) {
                Some((n, ..)) => {
                    used.insert(*n);
                }
                None => out.push(Finding { rule: NAME, path, line: Some(h.line), message: h.message }),
            }
        }
        for (n, p, w) in allow.0.iter().filter(|(n, ..)| !used.contains(n)) {
            out.push(Finding {
                rule: NAME,
                path: ALLOW_FILE.into(),
                line: Some(*n),
                message: format!("lists {p}:{w}, which no longer has a finding — remove the line (the list only shrinks)"),
            });
        }
        out.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "The control plane holds lifecycle state (state, tip, holder, holds, repo) keyed by bead id; \
bd holds the bead's title, body, labels, priority, type, comments and its dependency and parent edges. \
Read planning data through spira-lc's planning verbs instead of copying it, and name control-plane \
records lifecycle/<id> / LifecycleRecord. A new control-plane record type is registered in \
spira-lint/two-plane-records."
    }
}

pub fn rules() -> Vec<Box<dyn Rule>> {
    vec![Box::new(TwoPlane)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const RECORD: &str = "lifecycle/src/rec.rs";

    fn fixture(tag: &str) -> TempDir {
        let t = TempDir::new(tag);
        t.write(RECORD, "pub struct LifeRow {\n    pub id: String,\n    #[serde(default)]\n    pub holds: Vec<String>,\n}\n");
        t.write(RECORDS_FILE, &format!("# registry\n{RECORD}::LifeRow\n"));
        t.write(ALLOW_FILE, "");
        t
    }

    fn run(t: &TempDir, extra: &[&str]) -> Result<Vec<String>, LintError> {
        let files: Vec<&str> = [RECORD, RECORDS_FILE, ALLOW_FILE].into_iter().chain(extra.iter().copied()).collect();
        let tree = Tree::from_paths(t.path(), files, std::iter::empty::<&str>());
        TwoPlane.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_clean_control_plane_passes() {
        let t = fixture("tp-clean");
        t.write("lifecycle/schema.sql", "CREATE TABLE IF NOT EXISTS bead (\n    bead_id VARCHAR(64) NOT NULL,\n    state VARCHAR(16),\n    PRIMARY KEY (bead_id)\n);\n");
        assert_eq!(run(&t, &["lifecycle/schema.sql"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn an_edge_table_is_reported() {
        let t = fixture("tp-dep");
        t.write(
            "lifecycle/schema.sql",
            "-- the bead_dep mirror\nCREATE TABLE IF NOT EXISTS bead_dep (\n    bead_id VARCHAR(64) NOT NULL,\n    depends_on VARCHAR(64) NOT NULL\n);\n\
             CREATE TABLE `spira_lifecycle`.`epic_edge` (a INT);\nCREATE TABLE labels (a INT);\nALTER TABLE parked RENAME TO round_dep;\n",
        );
        assert_eq!(
            run(&t, &["lifecycle/schema.sql"]).unwrap(),
            [
                "two-plane: lifecycle/schema.sql:2: table bead_dep is an edge table: the dependency and parent graph belongs to bd",
                "two-plane: lifecycle/schema.sql:6: table epic_edge is an edge table: the dependency and parent graph belongs to bd",
                "two-plane: lifecycle/schema.sql:7: table labels is named for planning data, which belongs to bd",
                "two-plane: lifecycle/schema.sql:8: table round_dep is an edge table: the dependency and parent graph belongs to bd",
            ]
        );
    }

    #[test]
    fn a_copied_planning_column_is_reported() {
        let t = fixture("tp-col");
        t.write(
            "lifecycle/migrations/0099-x.sql",
            "-- ALTER TABLE bead ADD COLUMN body TEXT;\nALTER TABLE bead ADD COLUMN title VARCHAR(512) NULL;\nALTER TABLE bead ADD COLUMN since BIGINT;\n\
             ALTER TABLE bead ADD INDEX i (since),\n  ADD labels TEXT;\nALTER TABLE ask CHANGE COLUMN quote body TEXT;\nALTER TABLE batch RENAME COLUMN up TO parent;\n",
        );
        t.write("lifecycle/schema.sql", "CREATE TABLE bead (\n    bead_id VARCHAR(64),\n    priority TINYINT NULL,\n    PRIMARY KEY (bead_id)\n);\n");
        assert_eq!(
            run(&t, &["lifecycle/migrations/0099-x.sql", "lifecycle/schema.sql"]).unwrap(),
            [
                "two-plane: lifecycle/migrations/0099-x.sql:2: column bead.title copies planning data",
                "two-plane: lifecycle/migrations/0099-x.sql:5: column bead.labels copies planning data",
                "two-plane: lifecycle/migrations/0099-x.sql:6: column ask.body copies planning data",
                "two-plane: lifecycle/migrations/0099-x.sql:7: column batch.parent copies planning data",
                "two-plane: lifecycle/schema.sql:3: column bead.priority copies planning data",
            ]
        );
    }

    #[test]
    fn a_registered_record_with_a_planning_field_is_reported() {
        let t = fixture("tp-rec");
        t.write(
            RECORD,
            "pub struct LifeRow {\n    pub id: String,\n    pub priority: Option<u8>,\n    pub holds: BTreeMap<String, (u8, u8)>,\n    #[serde(default)] pub(crate) labels: Vec<String>,\n}\npub struct Other { pub title: String }\n",
        );
        assert_eq!(
            run(&t, &[]).unwrap(),
            ["two-plane: lifecycle/src/rec.rs:3: record LifeRow carries planning field priority", "two-plane: lifecycle/src/rec.rs:5: record LifeRow carries planning field labels"]
        );
    }

    #[test]
    fn a_registry_line_naming_a_missing_type_refuses() {
        let t = fixture("tp-missing");
        t.write(RECORDS_FILE, &format!("{RECORD}::LifeRow\n{RECORD}::GoneRow\n"));
        assert!(matches!(run(&t, &[]), Err(LintError::Refused(m)) if m.contains("GoneRow")));
        t.write(RECORDS_FILE, "lifecycle/src/nofile.rs::LifeRow\n");
        assert!(matches!(run(&t, &[]), Err(LintError::Refused(_))));
        t.write(RECORDS_FILE, "# nothing registered\n");
        assert!(matches!(run(&t, &[]), Err(LintError::Refused(_))));
    }

    #[test]
    fn a_bead_key_literal_or_type_in_control_code_is_reported() {
        let t = fixture("tp-key");
        t.write(
            "spira-lc/src/k.rs",
            "// a \"bead/x\" in prose is fine\nfn k(id: &str) -> String { format!(\"bead/{id}\") }\nconst P: &str = \"beads/\";\nconst L: &str = \"lifecycle/x\";\npub struct BeadStateRecord;\n",
        );
        assert_eq!(
            run(&t, &["spira-lc/src/k.rs"]).unwrap(),
            [
                format!("two-plane: spira-lc/src/k.rs:2: key literal \"bead/…\": {VOCABULARY}"),
                format!("two-plane: spira-lc/src/k.rs:3: key literal \"beads/…\": {VOCABULARY}"),
                format!("two-plane: spira-lc/src/k.rs:5: type BeadStateRecord: {VOCABULARY}"),
            ]
        );
    }

    #[test]
    fn sql_in_control_code_naming_planning_data_is_reported() {
        let t = fixture("tp-rsql");
        t.write(
            "spira-lc/src/q.rs",
            "fn a() { q(\"SELECT bead_id, depends_on FROM bead_dep\"); }\nfn b() { q(\"SELECT title FROM bead WHERE state = 'READY'\"); }\nfn c() { q(\"SELECT id, title FROM issues\"); }\n\
             const D: &str = r#\"SELECT bead_id,\n  priority\n  FROM x\"#;\nconst E: [&str; 3] = [\"bead_id\", \"state\",\n  \"title\"];\nfn f() { json!({\"bead_id\": id, \"body\": b}); }\n",
        );
        let carries = |n: usize, w: &str| format!("two-plane: spira-lc/src/q.rs:{n}: carries planning field {w} on a lifecycle record: read it through the planning verbs");
        assert_eq!(
            run(&t, &["spira-lc/src/q.rs"]).unwrap(),
            [
                carries(1, "depends_on"),
                "two-plane: spira-lc/src/q.rs:1: reads or writes edge table bead_dep: the dependency graph belongs to bd".to_string(),
                carries(2, "title"),
                carries(5, "priority"),
                carries(8, "title"),
                carries(9, "body"),
            ]
        );
    }

    #[test]
    fn an_allowlisted_finding_passes_and_a_stale_line_is_reported() {
        let t = fixture("tp-allow");
        t.write("lifecycle/schema.sql", "CREATE TABLE bead_dep (\n    bead_id VARCHAR(64)\n);\n");
        t.write(ALLOW_FILE, "# header\nlifecycle/schema.sql:bead_dep sp-x1\n");
        assert_eq!(run(&t, &["lifecycle/schema.sql"]).unwrap(), Vec::<String>::new());
        t.write(ALLOW_FILE, "lifecycle/schema.sql:bead_dep sp-x1\nlifecycle/schema.sql:bead.title sp-x1\n");
        assert_eq!(
            run(&t, &["lifecycle/schema.sql"]).unwrap(),
            ["two-plane: spira-lint/two-plane-allow:2: lists lifecycle/schema.sql:bead.title, which no longer has a finding — remove the line (the list only shrinks)"]
        );
        t.write(ALLOW_FILE, "lifecycle/schema.sql:bead_dep kept a reason in words\n");
        assert_eq!(run(&t, &["lifecycle/schema.sql"]).unwrap(), Vec::<String>::new());
        t.write("lifecycle/migrations/0001-a.sql", "CREATE TABLE bead_dep (a INT);\n");
        assert_eq!(
            run(&t, &["lifecycle/schema.sql", "lifecycle/migrations/0001-a.sql"]).unwrap(),
            ["two-plane: lifecycle/migrations/0001-a.sql:1: table bead_dep is an edge table: the dependency and parent graph belongs to bd"]
        );
        for bad in ["lifecycle/schema.sql:bead_dep\n", "lifecycle/schema.sql:bead_dep kept\n", "lifecycle/schema.sql:bead_dep sp-\n", "lifecycle/schema.sql bead_dep sp-x1\n"] {
            t.write(ALLOW_FILE, bad);
            assert!(matches!(run(&t, &["lifecycle/schema.sql"]), Err(LintError::BadAllow { line: 1, .. })), "{bad:?}");
        }
    }

    #[test]
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("tp-none");
        t.write("README.md", "bead_dep\n");
        let tree = Tree::from_paths(t.path(), ["README.md"], std::iter::empty::<&str>());
        assert_eq!(TwoPlane.check(&tree), Err(LintError::EmptyScope));
    }
}
