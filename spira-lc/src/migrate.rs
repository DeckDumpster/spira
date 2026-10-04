//! `spira-lc admin-migrate <dir-or-file>...` (sp-xfqnr): apply `lifecycle/migrations/*.sql`
//! to a `spira_lifecycle` database of any age, in filename order, so a fresh install and a
//! re-run converge on the same shape.
//!
//! WHY NOT JUST `admin-apply-ddl` EACH FILE. The migrations are written for an *existing*
//! database and are not all idempotent: Dolt has no `ADD COLUMN IF NOT EXISTS`, so
//! `0001-stack.sql`/`0002-since.sql` fail with "duplicate column" against a database
//! `schema.sql` just created (which already carries those columns) or one already migrated.
//!
//! WHAT THIS DOES INSTEAD — DETECTION, NOT A LEDGER. Each statement is classified:
//!   - `ALTER TABLE <t> ADD [COLUMN] <c> ...` runs only when `information_schema.columns`
//!     has no `<t>.<c>` in `spira_lifecycle` — the column's presence IS the record that the
//!     step was applied, whichever path (schema.sql or this migration) put it there;
//!   - any other `ALTER` is REFUSED before anything in the run executes: no other ALTER shape
//!     can be guarded this way, so a new migration of that kind needs this module taught
//!     first rather than a silent second application;
//!   - everything else (`0003-terminal-holder.sql`'s guarded UPDATE) runs as written, and
//!     must therefore be idempotent on its own — the rule a new migration file has to keep.
//!
//! A ledger table was rejected: a database `schema.sql` created fresh already has every
//! column, so a ledger would still need this same detection to know 0001/0002 are moot, and
//! it would be a second record that can disagree with the real table shape.

use std::path::{Path, PathBuf};

use crate::db::Conn;

/// One statement of a migration file, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    /// `ALTER TABLE <table> ADD [COLUMN] <column> ...` — applied only when the column is absent.
    AddColumn { table: String, column: String, sql: String },
    /// An ALTER this module cannot guard — refused.
    UnguardedAlter(String),
    /// Anything else — run as written (must be idempotent itself).
    Plain(String),
}

/// The migration files named by `args`, in the order they apply: every `*.sql` directly in a
/// directory argument plus every file argument, sorted by file name (the `NNNN-` prefix).
pub fn ordered_files(args: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for a in args {
        let p = Path::new(a);
        if p.is_dir() {
            let rd = std::fs::read_dir(p).map_err(|e| format!("reading {a}: {e}"))?;
            for e in rd.flatten() {
                let path = e.path();
                if path.is_file() && path.extension().is_some_and(|x| x == "sql") {
                    files.push(path);
                }
            }
        } else if p.is_file() {
            files.push(p.to_path_buf());
        } else {
            return Err(format!("no such migration file or directory: {a}"));
        }
    }
    sort_by_name(&mut files);
    Ok(files)
}

/// Filename order (`0001-…` before `0002-…` before `0010-…`), independent of directory.
pub fn sort_by_name(files: &mut [PathBuf]) {
    files.sort_by(|a, b| a.file_name().cmp(&b.file_name()).then_with(|| a.cmp(b)));
}

/// Split a SQL file into statements: `--` line comments dropped, split on `;` outside
/// single-quoted, double-quoted and backtick-quoted text, empty statements discarded.
pub fn split_statements(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == '\\' && q != '`' {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' | '`' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '-' if chars.peek() == Some(&'-') => {
                    for n in chars.by_ref() {
                        if n == '\n' {
                            cur.push('\n');
                            break;
                        }
                    }
                }
                ';' => {
                    let s = cur.trim().to_string();
                    if !s.is_empty() {
                        out.push(s);
                    }
                    cur.clear();
                }
                _ => cur.push(c),
            },
        }
    }
    let s = cur.trim().to_string();
    if !s.is_empty() {
        out.push(s);
    }
    out
}

fn ident(tok: &str) -> String {
    tok.trim_matches('`').to_string()
}

/// Classify one statement (see the module doc).
pub fn classify(sql: &str) -> Stmt {
    let toks: Vec<&str> = sql.split_whitespace().collect();
    let up = |i: usize| toks.get(i).map(|t| t.to_ascii_uppercase()).unwrap_or_default();
    if up(0) != "ALTER" {
        return Stmt::Plain(sql.to_string());
    }
    if up(1) == "TABLE" && toks.len() > 4 && up(3) == "ADD" {
        let (col_at, rest_ok) = if up(4) == "COLUMN" { (5, toks.len() > 6) } else { (4, toks.len() > 5) };
        // `ADD INDEX`/`ADD CONSTRAINT`/`ADD COLUMN IF ...` are not a plain column add.
        let col = up(col_at);
        let not_a_column = ["INDEX", "KEY", "UNIQUE", "PRIMARY", "CONSTRAINT", "FOREIGN", "FULLTEXT", "SPATIAL", "CHECK", "IF", "("];
        // One ADD per statement only: a comma-separated multi-ADD could be half-applied.
        if rest_ok && !not_a_column.contains(&col.as_str()) && !col.starts_with('(') && !sql.contains(',') {
            return Stmt::AddColumn { table: ident(toks[2]), column: ident(toks[col_at]), sql: sql.to_string() };
        }
    }
    Stmt::UnguardedAlter(sql.to_string())
}

/// Every statement of every file, in apply order, or the first refusal (file, statement).
pub fn plan(files: &[(String, String)]) -> Result<Vec<(String, Stmt)>, String> {
    let mut out = Vec::new();
    for (name, text) in files {
        for s in split_statements(text) {
            match classify(&s) {
                Stmt::UnguardedAlter(sql) => {
                    return Err(format!("{name}: refusing an ALTER that cannot be applied idempotently (only `ALTER TABLE t ADD COLUMN c ...` is guarded): {sql}"));
                }
                st => out.push((name.clone(), st)),
            }
        }
    }
    Ok(out)
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The probe that decides whether an `AddColumn` is already applied.
pub fn column_probe_sql(table: &str, column: &str) -> String {
    format!(
        "SELECT column_name FROM information_schema.columns WHERE table_schema = 'spira_lifecycle' AND table_name = {} AND column_name = {}",
        quote(table),
        quote(column)
    )
}

pub fn run(args: &[String], conn: &Conn) -> (i32, String) {
    if args.is_empty() {
        return (2, "admin-migrate: missing <migrations-dir-or-file>...".into());
    }
    let files = match ordered_files(args) {
        Ok(f) => f,
        Err(e) => return (2, format!("admin-migrate: {e}")),
    };
    let mut texts = Vec::new();
    for f in &files {
        let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        match std::fs::read_to_string(f) {
            Ok(t) => texts.push((name, t)),
            Err(e) => return (2, format!("admin-migrate: reading {}: {e}", f.display())),
        }
    }
    let steps = match plan(&texts) {
        Ok(s) => s,
        Err(e) => return (2, format!("admin-migrate: {e}")),
    };
    let mut report = Vec::new();
    for (name, st) in steps {
        match st {
            Stmt::AddColumn { table, column, sql } => match conn.query(&column_probe_sql(&table, &column)) {
                Ok(rows) if !rows.is_empty() => report.push(format!("{name}: {table}.{column} present — skipped")),
                Ok(_) => match conn.run_plain(&sql) {
                    Ok(()) => report.push(format!("{name}: added {table}.{column}")),
                    Err(e) => return (2, format!("admin-migrate: {name}: cannot tell: {e:?}")),
                },
                Err(e) => return (2, format!("admin-migrate: {name}: cannot tell: {e:?}")),
            },
            Stmt::Plain(sql) => match conn.run_plain(&sql) {
                Ok(()) => report.push(format!("{name}: applied")),
                Err(e) => return (2, format!("admin-migrate: {name}: cannot tell: {e:?}")),
            },
            Stmt::UnguardedAlter(_) => unreachable!("plan refuses these"),
        }
    }
    (0, report.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_apply_in_filename_order_whatever_order_they_are_named_in() {
        let d = testkit::TempDir::new("lc-migrate-order");
        for n in ["0010-later.sql", "0002-since.sql", "0001-stack.sql", "notes.txt"] {
            std::fs::write(d.path().join(n), "SELECT 1;").unwrap();
        }
        let got = ordered_files(&[d.path().to_string_lossy().to_string()]).unwrap();
        let names: Vec<String> = got.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
        assert_eq!(names, ["0001-stack.sql", "0002-since.sql", "0010-later.sql"]);
    }

    #[test]
    fn a_missing_argument_is_refused_not_skipped() {
        assert!(ordered_files(&["/nonexistent/lc-migrations".into()]).is_err());
    }

    #[test]
    fn the_real_migrations_plan_and_order() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../lifecycle/migrations");
        let files = ordered_files(&[dir.to_string()]).unwrap();
        let texts: Vec<(String, String)> = files.iter().map(|f| (f.file_name().unwrap().to_string_lossy().to_string(), std::fs::read_to_string(f).unwrap())).collect();
        assert_eq!(texts.iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["0001-stack.sql", "0002-since.sql", "0003-terminal-holder.sql"]);
        let steps = plan(&texts).unwrap();
        let adds: Vec<(String, String)> = steps
            .iter()
            .filter_map(|(_, s)| match s {
                Stmt::AddColumn { table, column, .. } => Some((table.clone(), column.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(adds, [("bead".to_string(), "stack".to_string()), ("bead".into(), "stack_depth".into()), ("bead".into(), "since".into())]);
        let plain = steps.iter().filter(|(_, s)| matches!(s, Stmt::Plain(_))).count();
        assert_eq!(plain, 1, "0003's guarded UPDATE runs as written");
    }

    #[test]
    fn comments_and_quoted_semicolons_do_not_split() {
        let s = split_statements("-- a; comment\nUPDATE t SET a = 'x;y' -- trailing; note\n WHERE b = 1;\n\n-- only a comment\n");
        assert_eq!(s.len(), 1, "{s:?}");
        assert!(s[0].contains("'x;y'") && s[0].contains("WHERE b = 1"));
    }

    #[test]
    fn add_column_is_guarded_with_or_without_the_column_keyword() {
        assert_eq!(classify("ALTER TABLE bead ADD COLUMN since BIGINT NULL"), Stmt::AddColumn { table: "bead".into(), column: "since".into(), sql: "ALTER TABLE bead ADD COLUMN since BIGINT NULL".into() });
        assert!(matches!(classify("alter table `bead` add `x` INT"), Stmt::AddColumn { ref table, ref column, .. } if table == "bead" && column == "x"));
    }

    #[test]
    fn every_other_alter_is_refused_before_anything_runs() {
        for sql in ["ALTER TABLE bead DROP COLUMN since", "ALTER TABLE bead ADD INDEX i (state)", "ALTER TABLE bead ADD COLUMN a INT, ADD COLUMN b INT", "ALTER TABLE bead RENAME COLUMN a TO b", "ALTER TABLE bead ADD CONSTRAINT c CHECK (1)"] {
            assert!(matches!(classify(sql), Stmt::UnguardedAlter(_)), "{sql}");
        }
        let files = vec![("0001.sql".to_string(), "UPDATE t SET a = 1;".to_string()), ("0002.sql".to_string(), "ALTER TABLE bead DROP COLUMN x;".to_string())];
        let e = plan(&files).unwrap_err();
        assert!(e.contains("0002.sql"), "{e}");
    }

    #[test]
    fn the_probe_quotes_its_identifiers() {
        let q = column_probe_sql("bead", "it's");
        assert!(q.contains("table_name = 'bead'") && q.contains("column_name = 'it\\'s'"), "{q}");
    }
}
