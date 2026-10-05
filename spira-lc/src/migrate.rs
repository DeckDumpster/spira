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
//!   - everything else (`0003-terminal-holder.sql`'s guarded UPDATE) runs as written when its
//!     probe says it is pending, and must therefore be idempotent on its own — the rule a new
//!     migration file has to keep. A statement no read can confirm ([`Probe::Unprobeable`])
//!     is always pending, so it would need the admin on every activation: write it guarded.
//!
//! WHO PROBES, WHO APPLIES (sp-p1z81). "Already applied" is decided read-only, by the
//! connection the environment names for every other verb (`SPIRA_LC_USER` +
//! `SPIRA_LC_PASSWORD_FILE`, the lifecycle service user lc-serve uses) — see [`Probe`]: a
//! column in `information_schema.columns`, a guarded UPDATE/DELETE whose own WHERE no longer
//! matches a row, a table or index in `information_schema`. When nothing is pending the run
//! succeeds with no admin credential at all (release pre-activate on every flip). Only a
//! pending step is applied, as `SPIRA_LC_ADMIN_USER`/`SPIRA_LC_ADMIN_PASSWORD`; with those
//! absent or refused, the run fails naming exactly those two variables and the pending file.
//!
//! A ledger table was rejected: a database `schema.sql` created fresh already has every
//! column, so a ledger would still need this same detection to know 0001/0002 are moot, and
//! it would be a second record that can disagree with the real table shape.

use std::path::{Path, PathBuf};

use serde_json::Value;

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

/// The probe behind an `AddColumn`: every column `table` has, as this user sees them.
pub fn table_columns_sql(table: &str) -> String {
    format!("SELECT column_name FROM information_schema.columns WHERE table_schema = 'spira_lifecycle' AND table_name = {}", quote(table))
}

/// `--if-enforced` (sp-vf9iu): release pre-activate runs admin-migrate on every activation,
/// including on installs with no lifecycle store (`lifecycle_enforce` off), where there is
/// nothing to migrate. Returns the remaining arguments, or `None` when the flag is present
/// and enforcement is off (nothing to do).
pub fn gate_if_enforced(args: &[String], enforced: impl FnOnce() -> bool) -> Option<Vec<String>> {
    let rest: Vec<String> = args.iter().filter(|a| *a != "--if-enforced").cloned().collect();
    if rest.len() != args.len() && !enforced() {
        return None;
    }
    Some(rest)
}

/// The database a migration step is probed and applied through: a live [`Conn`] in
/// production, a fake in the unit tests.
pub trait Sql {
    fn rows(&self, sql: &str) -> Result<Vec<Value>, String>;
    fn exec(&self, sql: &str) -> Result<(), String>;
}

impl Sql for Conn {
    fn rows(&self, sql: &str) -> Result<Vec<Value>, String> {
        self.query(sql).map_err(|e| format!("{e:?}"))
    }
    fn exec(&self, sql: &str) -> Result<(), String> {
        self.run_plain(sql).map_err(|e| format!("{e:?}"))
    }
}

/// How a step's "already applied" is decided — read-only, never by running DDL (sp-p1z81):
/// the lifecycle service user can answer every probe, so a database that already carries
/// every migration needs no admin credential at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// `ALTER TABLE t ADD COLUMN c`: applied when `information_schema.columns` lists `t.c`.
    Column { table: String, column: String },
    /// A guarded `UPDATE t SET ... WHERE w` / `DELETE FROM t WHERE w`: applied when no row
    /// still matches `w` (the statement would change nothing) — this SELECT finds one.
    NoRowMatches(String),
    /// `CREATE TABLE [IF NOT EXISTS] t`: applied when `information_schema.tables` lists `t`.
    Table(String),
    /// `CREATE [UNIQUE] INDEX i ON t`: applied when `information_schema.statistics` lists it.
    Index { table: String, index: String },
    /// No read can tell (an UPDATE with no WHERE, an INSERT, ...): always pending, so a
    /// migration of that shape needs the admin on every activation — write it guarded.
    Unprobeable,
}

/// The byte offset of the first top-level ` WHERE ` keyword (outside quotes), if any.
fn where_at(sql: &str) -> Option<usize> {
    let b = sql.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) => {
                if c == b'\\' && q != b'`' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            }
            None if c == b'\'' || c == b'"' || c == b'`' => quote = Some(c),
            None => {
                let before_ok = i > 0 && b[i - 1].is_ascii_whitespace();
                let after_ok = b.get(i + 5).is_none_or(|n| n.is_ascii_whitespace() || *n == b'(');
                if before_ok && after_ok && sql.get(i..i + 5).is_some_and(|w| w.eq_ignore_ascii_case("WHERE")) {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

fn plain_ident(tok: &str) -> Option<String> {
    let t = ident(tok);
    (!t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(t)
}

/// The read that decides whether `st` is already applied (see [`Probe`]).
pub fn probe_for(st: &Stmt) -> Probe {
    let sql = match st {
        Stmt::AddColumn { table, column, .. } => return Probe::Column { table: table.clone(), column: column.clone() },
        Stmt::UnguardedAlter(_) => return Probe::Unprobeable,
        Stmt::Plain(sql) => sql.as_str(),
    };
    let toks: Vec<&str> = sql.split_whitespace().collect();
    let up = |i: usize| toks.get(i).map(|t| t.to_ascii_uppercase()).unwrap_or_default();
    let guarded = |table: &str| match where_at(sql) {
        Some(w) => Probe::NoRowMatches(format!("SELECT 1 FROM {table} {} LIMIT 1", sql[w..].trim())),
        None => Probe::Unprobeable,
    };
    match (up(0).as_str(), up(1).as_str()) {
        ("UPDATE", _) if up(2) == "SET" => plain_ident(toks[1]).map_or(Probe::Unprobeable, |t| guarded(&t)),
        ("DELETE", "FROM") if toks.len() > 3 && up(3) == "WHERE" => plain_ident(toks[2]).map_or(Probe::Unprobeable, |t| guarded(&t)),
        ("CREATE", "TABLE") => {
            let at = if up(2) == "IF" && up(3) == "NOT" && up(4) == "EXISTS" { 5 } else { 2 };
            toks.get(at).map(|t| t.split('(').next().unwrap_or("")).and_then(plain_ident).map_or(Probe::Unprobeable, Probe::Table)
        }
        ("CREATE", "INDEX") | ("CREATE", "UNIQUE") => {
            let at = if up(1) == "UNIQUE" { 3 } else { 2 };
            if (up(1) == "UNIQUE" && up(2) != "INDEX") || up(at + 1) != "ON" {
                return Probe::Unprobeable;
            }
            match (toks.get(at).and_then(|t| plain_ident(t)), toks.get(at + 2).map(|t| t.split('(').next().unwrap_or("")).and_then(plain_ident)) {
                (Some(index), Some(table)) => Probe::Index { table, index },
                _ => Probe::Unprobeable,
            }
        }
        _ => Probe::Unprobeable,
    }
}

fn any_cell_is(rows: &[Value], want: &str) -> bool {
    rows.iter().any(|r| r.as_object().is_some_and(|o| o.values().any(|v| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case(want)))))
}

/// Whether `st` is already applied, read through `db`. An `Err` is "cannot tell".
pub fn is_applied(db: &dyn Sql, st: &Stmt) -> Result<bool, String> {
    match probe_for(st) {
        Probe::Column { table, column } => {
            let rows = db.rows(&table_columns_sql(&table))?;
            // A table with no visible columns is absent or hidden from this user — either
            // way a missing column cannot be concluded from it.
            if rows.is_empty() {
                return Err(format!("information_schema shows no columns of {table} to this user"));
            }
            Ok(any_cell_is(&rows, &column))
        }
        Probe::NoRowMatches(q) => Ok(db.rows(&q)?.is_empty()),
        Probe::Table(t) => Ok(!db.rows(&format!("SELECT table_name FROM information_schema.tables WHERE table_schema = 'spira_lifecycle' AND table_name = {}", quote(&t)))?.is_empty()),
        Probe::Index { table, index } => Ok(!db
            .rows(&format!("SELECT index_name FROM information_schema.statistics WHERE table_schema = 'spira_lifecycle' AND table_name = {} AND index_name = {}", quote(&table), quote(&index)))?
            .is_empty()),
        Probe::Unprobeable => Ok(false),
    }
}

/// What the probe saw, for the report: the applied wording or the pending one.
fn what(st: &Stmt, applied: bool) -> String {
    let (yes, no) = match probe_for(st) {
        Probe::Column { table, column } => (format!("{table}.{column} present"), format!("{table}.{column} absent")),
        Probe::NoRowMatches(_) => ("no row left for it to change".into(), "rows it still has to change".into()),
        Probe::Table(t) => (format!("table {t} present"), format!("table {t} absent")),
        Probe::Index { table, index } => (format!("index {table}.{index} present"), format!("index {table}.{index} absent")),
        Probe::Unprobeable => (String::new(), "a statement no read can confirm".into()),
    };
    if applied { yes } else { no }
}

/// The environment variables a pending migration is applied as — named, never echoed, in
/// every refusal (law-a-refusal-names-its-exit).
pub const ADMIN_VARS: &str = "SPIRA_LC_ADMIN_USER and SPIRA_LC_ADMIN_PASSWORD";

/// Probe every step as the service user; when one is pending, apply it and every later
/// pending step as `admin` — or refuse naming [`ADMIN_VARS`] when there is no admin.
pub fn migrate(steps: &[(String, Stmt)], service: &dyn Sql, admin: Option<&dyn Sql>) -> (i32, String) {
    let mut report = Vec::new();
    // Phase 1, read-only, as the service user: everything before the first pending step.
    // Later steps are decided only after it applies (a probe may read what it adds).
    let mut first_pending = None;
    for (i, (name, st)) in steps.iter().enumerate() {
        match is_applied(service, st) {
            Ok(true) => report.push(format!("{name}: {} — skipped", what(st, true))),
            Ok(false) => {
                first_pending = Some(i);
                break;
            }
            Err(e) => return (2, format!("admin-migrate: {name}: cannot tell whether it is applied (read as the lifecycle service user): {e}")),
        }
    }
    let Some(from) = first_pending else {
        report.push(format!("admin-migrate: every migration already applied ({} step(s)) — no admin needed", steps.len()));
        return (0, report.join("\n"));
    };
    let (name, st) = &steps[from];
    let Some(admin) = admin else {
        return (
            2,
            format!(
                "admin-migrate: {name} is pending ({}) and applying it needs a database admin — set {ADMIN_VARS} (no admin credential is configured)",
                what(st, false)
            ),
        );
    };
    // Phase 2, as the admin: re-probe each remaining step on the admin's own session (it
    // sees its own earlier writes) and apply only what is still pending.
    let refused = |name: &str, e: &str| format!("admin-migrate: {name}: pending, and applying it as the database admin failed: {e} — check {ADMIN_VARS}");
    for (name, st) in &steps[from..] {
        match is_applied(admin, st) {
            Ok(true) => report.push(format!("{name}: {} — skipped", what(st, true))),
            Ok(false) => {
                let sql = match st {
                    Stmt::AddColumn { sql, .. } | Stmt::Plain(sql) => sql,
                    Stmt::UnguardedAlter(_) => unreachable!("plan refuses these"),
                };
                match admin.exec(sql) {
                    Ok(()) => report.push(match st {
                        Stmt::AddColumn { table, column, .. } => format!("{name}: added {table}.{column}"),
                        _ => format!("{name}: applied"),
                    }),
                    Err(e) => return (2, refused(name, &e)),
                }
            }
            Err(e) => return (2, refused(name, &e)),
        }
    }
    (0, report.join("\n"))
}

/// The admin credential from the environment: `SPIRA_LC_ADMIN_USER` (non-empty) with
/// `SPIRA_LC_ADMIN_PASSWORD` — or, for spira-install, which keeps the admin password out of
/// every environment, the file `SPIRA_LC_ADMIN_PASSWORD_FILE` names. `None` when no user.
pub fn admin_from_env(get: impl Fn(&str) -> Option<String>) -> Result<Option<(String, String)>, String> {
    let Some(user) = get("SPIRA_LC_ADMIN_USER").filter(|u| !u.is_empty()) else {
        return Ok(None);
    };
    let password = crate::db::password_from(get("SPIRA_LC_ADMIN_PASSWORD_FILE"), get("SPIRA_LC_ADMIN_PASSWORD"), |p| std::fs::read_to_string(p))?;
    Ok(Some((user, password)))
}

pub fn run(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(args) = gate_if_enforced(args, || spira_config::lifecycle_enforce(None)) else {
        return (0, "admin-migrate: lifecycle_enforce is off — no lifecycle store to migrate".into());
    };
    let args = &args[..];
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
    let admin = match admin_from_env(|k| std::env::var(k).ok()) {
        Ok(a) => a.map(|(user, password)| conn.as_user(user, password)),
        Err(e) => return (2, format!("admin-migrate: the admin password file (SPIRA_LC_ADMIN_PASSWORD_FILE): {e}")),
    };
    migrate(&steps, conn, admin.as_ref().map(|a| a as &dyn Sql))
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
    fn if_enforced_skips_only_when_enforcement_is_off() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(gate_if_enforced(&a(&["--if-enforced", "m"]), || false), None);
        assert_eq!(gate_if_enforced(&a(&["--if-enforced", "m"]), || true), Some(a(&["m"])));
        // Without the flag enforcement is never consulted: an explicit run always runs.
        assert_eq!(gate_if_enforced(&a(&["m"]), || panic!("consulted")), Some(a(&["m"])));
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
        let q = table_columns_sql("it's");
        assert!(q.contains("table_name = 'it\\'s'"), "{q}");
    }

    // ── sp-p1z81: "already applied" is read as the service user; only pending needs admin ──

    use std::cell::RefCell;
    use std::rc::Rc;

    /// One fake database seen through one user's connection.
    #[derive(Default)]
    struct State {
        columns: std::collections::BTreeSet<(String, String)>,
        /// Terminal rows still carrying a holder (what 0003's guarded UPDATE corrects).
        stale_terminal_rows: bool,
        /// Every statement that changed something, as `<user>: <sql>`.
        writes: Vec<String>,
    }
    struct FakeConn {
        user: &'static str,
        state: Rc<RefCell<State>>,
        /// The server refuses this user anything (wrong password).
        refused: bool,
        /// The user may read but not write (the service user's grants on a DDL).
        read_only: bool,
    }
    impl Sql for FakeConn {
        fn rows(&self, sql: &str) -> Result<Vec<Value>, String> {
            if self.refused {
                return Err(format!("Access denied for user '{}'", self.user));
            }
            let st = self.state.borrow();
            if sql.contains("information_schema.columns") {
                let table = sql.split("table_name = '").nth(1).and_then(|r| r.split('\'').next()).unwrap_or("");
                return Ok(st.columns.iter().filter(|(t, _)| t == table).map(|(_, c)| serde_json::json!({ "COLUMN_NAME": c })).collect());
            }
            if sql.starts_with("SELECT 1 FROM bead WHERE") {
                return Ok(if st.stale_terminal_rows { vec![serde_json::json!({ "1": "1" })] } else { vec![] });
            }
            Err(format!("fake: unexpected read {sql}"))
        }
        fn exec(&self, sql: &str) -> Result<(), String> {
            if self.refused {
                return Err(format!("Access denied for user '{}'", self.user));
            }
            if self.read_only {
                return Err(format!("command denied to user '{}'", self.user));
            }
            let mut st = self.state.borrow_mut();
            match classify(sql) {
                Stmt::AddColumn { table, column, .. } => {
                    if !st.columns.insert((table, column)) {
                        return Err("duplicate column".into());
                    }
                }
                _ => st.stale_terminal_rows = false,
            }
            st.writes.push(format!("{}: {sql}", self.user));
            Ok(())
        }
    }

    fn real_steps() -> Vec<(String, Stmt)> {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../lifecycle/migrations");
        let files = ordered_files(&[dir.to_string()]).unwrap();
        plan(&files.iter().map(|f| (f.file_name().unwrap().to_string_lossy().to_string(), std::fs::read_to_string(f).unwrap())).collect::<Vec<_>>()).unwrap()
    }

    /// A store with every shipped migration's effect already present (production today).
    fn migrated() -> Rc<RefCell<State>> {
        let mut st = State::default();
        for c in ["id", "state", "holder", "stack", "stack_depth", "since"] {
            st.columns.insert(("bead".into(), c.into()));
        }
        Rc::new(RefCell::new(st))
    }

    fn service(state: &Rc<RefCell<State>>) -> FakeConn {
        FakeConn { user: "spira_lc", state: state.clone(), refused: false, read_only: true }
    }

    #[test]
    fn every_migration_applied_needs_no_admin_and_writes_nothing() {
        let db = migrated();
        let (rc, out) = migrate(&real_steps(), &service(&db), None);
        assert_eq!(rc, 0, "{out}");
        assert!(out.contains("0002-since.sql: bead.since present"), "{out}");
        assert!(out.contains("0003-terminal-holder.sql"), "{out}");
        assert!(db.borrow().writes.is_empty(), "nothing ran: {:?}", db.borrow().writes);
    }

    #[test]
    fn a_pending_migration_with_no_admin_refuses_naming_the_admin_variables() {
        let db = migrated();
        db.borrow_mut().columns.remove(&("bead".into(), "since".into()));
        let (rc, out) = migrate(&real_steps(), &service(&db), None);
        assert_eq!(rc, 2, "{out}");
        assert!(out.contains("SPIRA_LC_ADMIN_USER") && out.contains("SPIRA_LC_ADMIN_PASSWORD"), "names the exit: {out}");
        assert!(out.contains("0002-since.sql") && out.contains("pending"), "names the pending migration: {out}");
        assert!(db.borrow().writes.is_empty());
    }

    #[test]
    fn a_pending_guarded_update_is_pending_too() {
        let db = migrated();
        db.borrow_mut().stale_terminal_rows = true;
        let (rc, out) = migrate(&real_steps(), &service(&db), None);
        assert_eq!(rc, 2, "{out}");
        assert!(out.contains("0003-terminal-holder.sql") && out.contains("SPIRA_LC_ADMIN_USER"), "{out}");
    }

    #[test]
    fn a_pending_migration_is_applied_as_the_admin() {
        let db = migrated();
        db.borrow_mut().columns.remove(&("bead".into(), "since".into()));
        db.borrow_mut().stale_terminal_rows = true;
        let admin = FakeConn { user: "root", state: db.clone(), refused: false, read_only: false };
        let (rc, out) = migrate(&real_steps(), &service(&db), Some(&admin));
        assert_eq!(rc, 0, "{out}");
        assert!(out.contains("0002-since.sql: added bead.since"), "{out}");
        let w = db.borrow().writes.clone();
        assert_eq!(w.len(), 2, "0002 and 0003 only, 0001 untouched: {w:?}");
        assert!(w.iter().all(|s| s.starts_with("root: ")), "{w:?}");
        // And the next activation needs no admin.
        let (rc, out) = migrate(&real_steps(), &service(&db), None);
        assert_eq!(rc, 0, "{out}");
    }

    #[test]
    fn a_refused_admin_names_the_variables_and_never_a_password() {
        let db = migrated();
        db.borrow_mut().columns.remove(&("bead".into(), "since".into()));
        let admin = FakeConn { user: "root", state: db.clone(), refused: true, read_only: false };
        let (rc, out) = migrate(&real_steps(), &service(&db), Some(&admin));
        assert_eq!(rc, 2, "{out}");
        assert!(out.contains("SPIRA_LC_ADMIN_USER") && out.contains("SPIRA_LC_ADMIN_PASSWORD"), "{out}");
        assert!(out.contains("0002-since.sql"), "{out}");
        assert!(db.borrow().writes.is_empty());
    }

    #[test]
    fn a_service_user_that_cannot_see_the_table_is_cannot_tell_not_pending() {
        let db = Rc::new(RefCell::new(State::default()));
        let (rc, out) = migrate(&real_steps(), &service(&db), None);
        assert_eq!(rc, 2, "{out}");
        assert!(out.contains("cannot tell") && out.contains("0001-stack.sql"), "{out}");
    }

    #[test]
    fn probes_read_and_never_alter() {
        let steps = real_steps();
        assert_eq!(probe_for(&steps[0].1), Probe::Column { table: "bead".into(), column: "stack".into() });
        match probe_for(&steps[3].1) {
            Probe::NoRowMatches(q) => {
                assert!(q.starts_with("SELECT 1 FROM bead WHERE state IN"), "{q}");
                assert!(q.contains("holder IS NOT NULL") && q.ends_with("LIMIT 1"), "{q}");
            }
            p => panic!("0003 is probed by its own WHERE: {p:?}"),
        }
        assert_eq!(probe_for(&Stmt::Plain("UPDATE t SET a = 1".into())), Probe::Unprobeable, "no WHERE: no read can tell");
        assert_eq!(probe_for(&Stmt::Plain("UPDATE t SET a = 'x where y' WHERE b = 1".into())), Probe::NoRowMatches("SELECT 1 FROM t WHERE b = 1 LIMIT 1".into()));
        assert_eq!(probe_for(&Stmt::Plain("DELETE FROM t WHERE b = 1".into())), Probe::NoRowMatches("SELECT 1 FROM t WHERE b = 1 LIMIT 1".into()));
        assert_eq!(probe_for(&Stmt::Plain("CREATE TABLE IF NOT EXISTS t2 (a INT)".into())), Probe::Table("t2".into()));
        assert_eq!(probe_for(&Stmt::Plain("CREATE UNIQUE INDEX i ON bead (state)".into())), Probe::Index { table: "bead".into(), index: "i".into() });
        assert_eq!(probe_for(&Stmt::Plain("INSERT INTO t VALUES (1)".into())), Probe::Unprobeable);
    }

    #[test]
    fn the_admin_comes_only_from_the_admin_variables() {
        let env = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        assert_eq!(admin_from_env(env(&[("SPIRA_LC_USER", "root"), ("SPIRA_LC_PASSWORD", "x")])).unwrap(), None);
        assert_eq!(admin_from_env(env(&[("SPIRA_LC_ADMIN_USER", "")])).unwrap(), None);
        assert_eq!(admin_from_env(env(&[("SPIRA_LC_ADMIN_USER", "root"), ("SPIRA_LC_ADMIN_PASSWORD", "pw")])).unwrap(), Some(("root".into(), "pw".into())));
        assert_eq!(admin_from_env(env(&[("SPIRA_LC_ADMIN_USER", "root")])).unwrap(), Some(("root".into(), String::new())));
    }
}
