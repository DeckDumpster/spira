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
//! succeeds with no admin credential at all (release pre-activate on every flip).
//!
//! A pending step is applied by whoever its migration FILE needs ([`Applier`]): a file made
//! only of guarded DML — `UPDATE t SET ... WHERE ...` / `DELETE FROM t WHERE ...` — on tables
//! `lifecycle/grants.sql` lets the service user write that way (UPDATE on bead, delivery,
//! batch, batch_member; DELETE on none) is applied by the service user itself and re-verified
//! by its probe, since lc-serve already writes those rows in normal operation (0003 is one).
//! Any other file — DDL (ALTER/CREATE/DROP/GRANT), an unguarded write, an INSERT, a table
//! outside those grants — is applied as `SPIRA_LC_ADMIN_USER`/`SPIRA_LC_ADMIN_PASSWORD`. With
//! those absent or refused — or the service user refused its DML (a privilege error) and no
//! admin to fall back to — the run fails naming exactly those two variables and the pending
//! file. A credential is never printed.
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

/// `--if-enforced` (sp-vf9iu) once skipped the migration on an install with the lifecycle
/// switch off. There is no off (sp-v62vn): the flag is accepted and ignored, so an older
/// pre-activate that still passes it migrates like any other caller.
pub fn strip_retired_flags(args: &[String]) -> Vec<String> {
    args.iter().filter(|a| *a != "--if-enforced").cloned().collect()
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
    /// `CREATE VIEW v`: applied when `information_schema.tables` lists it as a VIEW (the `views`
    /// table needs SHOW VIEW, which the service user does not hold).
    View(String),
    /// `CREATE OR REPLACE VIEW v AS SELECT ... AS c FROM ...`: applied when `SHOW COLUMNS FROM v`
    /// lists `c`, the last alias of its select list (information_schema.columns does not show
    /// a view's columns to the service user).
    ViewColumn { view: String, column: String },
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

/// The identifier after the last top-level `AS` of the select list (before the first top-level
/// `FROM`), skipping quotes and parenthesised subqueries.
fn last_select_alias(sql: &str) -> Option<String> {
    let (mut depth, mut quote, mut alias) = (0i32, None::<char>, None);
    let mut word = String::new();
    let mut prev = String::new();
    for c in sql.chars().chain(std::iter::once(' ')) {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c);
            continue;
        }
        if !word.is_empty() && depth == 0 {
            if word.eq_ignore_ascii_case("FROM") {
                return alias;
            }
            if prev.eq_ignore_ascii_case("AS") {
                alias = Some(word.clone());
            }
        }
        if !word.is_empty() {
            prev = std::mem::take(&mut word);
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
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
        ("CREATE", "OR") if up(2) == "REPLACE" && up(3) == "VIEW" => match (toks.get(4).and_then(|t| plain_ident(t)), last_select_alias(sql)) {
            (Some(view), Some(column)) => Probe::ViewColumn { view, column },
            _ => Probe::Unprobeable,
        },
        ("CREATE", "VIEW") => toks.get(2).and_then(|t| plain_ident(t)).map_or(Probe::Unprobeable, Probe::View),
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
        Probe::View(v) => Ok(!db.rows(&format!("SELECT table_name FROM information_schema.tables WHERE table_schema = 'spira_lifecycle' AND table_type = 'VIEW' AND table_name = {}", quote(&v)))?.is_empty()),
        Probe::ViewColumn { view, column } => Ok(any_cell_is(&db.rows(&format!("SHOW COLUMNS FROM {view}"))?, &column)),
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
        Probe::View(v) => (format!("view {v} present"), format!("view {v} absent")),
        Probe::ViewColumn { view, column } => (format!("view {view}.{column} present"), format!("view {view}.{column} absent")),
        Probe::Index { table, index } => (format!("index {table}.{index} present"), format!("index {table}.{index} absent")),
        Probe::Unprobeable => (String::new(), "a statement no read can confirm".into()),
    };
    if applied { yes } else { no }
}

/// The environment variables a pending migration is applied as — named, never echoed, in
/// every refusal (law-a-refusal-names-its-exit).
pub const ADMIN_VARS: &str = "SPIRA_LC_ADMIN_USER and SPIRA_LC_ADMIN_PASSWORD";

/// The grants production gives the lifecycle service user — the same file install applies,
/// so "which tables the service user writes" has one source, not a list kept beside it.
const GRANTS_SQL: &str = include_str!("../../lifecycle/grants.sql");

/// The service user's grantee, as grants.sql spells it.
const SERVICE_GRANTEE: &str = "'spira_lc'@'%'";

/// Whether `grants` gives the service user privilege `privilege` (`UPDATE`, `DELETE`) on
/// `spira_lifecycle.<table>`. Only single-line `GRANT p, q ON spira_lifecycle.t TO
/// 'spira_lc'@'%'` statements count; anything else grants nothing here.
pub fn service_may(grants: &str, privilege: &str, table: &str) -> bool {
    grants.lines().any(|line| {
        let line = line.trim().trim_end_matches(';').trim();
        let Some(rest) = line.strip_prefix("GRANT ") else { return false };
        let Some((privs, rest)) = rest.split_once(" ON ") else { return false };
        let Some((object, grantee)) = rest.split_once(" TO ") else { return false };
        let Some(t) = object.trim().strip_prefix("spira_lifecycle.") else { return false };
        grantee.trim() == SERVICE_GRANTEE && ident(t) == table && privs.split(',').any(|p| p.trim().eq_ignore_ascii_case(privilege))
    })
}

/// The `GRANT ... ON spira_lifecycle.<table> TO ...` statements of `grants` (single-line, no
/// placeholders), each with its table and grantee user.
pub fn table_grants(grants: &str) -> Vec<(String, String, String)> {
    grants
        .lines()
        .filter_map(|line| {
            let stmt = line.trim().trim_end_matches(';').trim();
            let rest = stmt.strip_prefix("GRANT ")?;
            let (_, rest) = rest.split_once(" ON ")?;
            let (object, grantee) = rest.split_once(" TO ")?;
            let table = object.trim().strip_prefix("spira_lifecycle.")?;
            let user = grantee.trim().trim_start_matches('\'').split('\'').next()?;
            Some((ident(table), user.to_string(), stmt.to_string()))
        })
        .collect()
}

/// Apply every grant in `grants` whose table and grantee exist, as `admin`; GRANT is idempotent.
/// A table a pending migration has yet to create is skipped until it has run, and a grantee the
/// server does not have (a world that never created the service users) has nothing to grant to.
fn apply_grants(admin: &dyn Sql, grants: &str) -> Result<(), String> {
    for (table, user, stmt) in table_grants(grants) {
        let probe = format!("SELECT table_name FROM information_schema.tables WHERE table_schema = 'spira_lifecycle' AND table_name = {}", quote(&table));
        if admin.rows(&probe).map_err(|e| format!("cannot tell whether {table} exists: {e}"))?.is_empty() {
            continue;
        }
        let who = format!("SELECT user FROM mysql.user WHERE user = {}", quote(&user));
        if admin.rows(&who).map_err(|e| format!("cannot tell whether user {user} exists: {e}"))?.is_empty() {
            continue;
        }
        admin.exec(&stmt).map_err(|e| format!("`{stmt}` failed: {e}"))?;
    }
    Ok(())
}

/// Who applies a pending migration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applier {
    /// Every statement is guarded DML (`UPDATE t SET ... WHERE ...` / `DELETE FROM t WHERE
    /// ...`) on a table grants.sql lets the service user write that way: lc-serve's own
    /// connection applies it, no admin needed.
    Service,
    /// Anything else — DDL (ALTER/CREATE/DROP/GRANT), an unguarded write, an INSERT, a
    /// table the service user may not write — needs [`ADMIN_VARS`].
    Admin,
}

/// Whether one statement is guarded DML the service user's grants (`grants`) cover.
pub fn service_can_apply(grants: &str, st: &Stmt) -> bool {
    let Stmt::Plain(sql) = st else { return false };
    let toks: Vec<&str> = sql.split_whitespace().collect();
    let up = |i: usize| toks.get(i).map(|t| t.to_ascii_uppercase()).unwrap_or_default();
    if where_at(sql).is_none() {
        return false;
    }
    let (privilege, table) = match up(0).as_str() {
        "UPDATE" if up(2) == "SET" => ("UPDATE", toks.get(1)),
        "DELETE" if up(1) == "FROM" && up(3) == "WHERE" => ("DELETE", toks.get(2)),
        _ => return false,
    };
    table.and_then(|t| plain_ident(t)).is_some_and(|t| service_may(grants, privilege, &t))
}

/// Who applies migration file `name`: [`Applier::Service`] only when every one of its
/// statements is guarded DML the service user may run; one DDL statement makes it Admin.
pub fn applier_for(grants: &str, steps: &[(String, Stmt)], name: &str) -> Applier {
    let mut of_file = steps.iter().filter(|(n, _)| n == name).peekable();
    if of_file.peek().is_some() && of_file.all(|(_, st)| service_can_apply(grants, st)) { Applier::Service } else { Applier::Admin }
}

/// A server refusal on privileges (`... command denied to user ...`, `Access denied ...`),
/// as opposed to a failing statement.
fn is_privilege_error(e: &str) -> bool {
    e.to_ascii_lowercase().contains("denied")
}

/// Probe every step as the service user; apply each pending step as its [`Applier`]: a
/// guarded-DML migration as the service user itself (re-verified by its probe afterwards),
/// anything else as `admin` — or refuse naming [`ADMIN_VARS`] when there is no admin, or
/// when the service user is refused the DML and there is no admin to fall back to.
pub fn migrate(steps: &[(String, Stmt)], service: &dyn Sql, admin: Option<&dyn Sql>) -> (i32, String) {
    migrate_with(GRANTS_SQL, steps, service, admin)
}

pub fn migrate_with(grants: &str, steps: &[(String, Stmt)], service: &dyn Sql, admin: Option<&dyn Sql>) -> (i32, String) {
    let mut report = Vec::new();
    let needs_admin = |name: &str, st: &Stmt, why: &str| {
        (2, format!("admin-migrate: {name} is pending ({}) and applying it needs a database admin — set {ADMIN_VARS} (no admin credential is configured{why})", what(st, false)))
    };
    let refused = |name: &str, e: &str| (2, format!("admin-migrate: {name}: pending, and applying it as the database admin failed: {e} — check {ADMIN_VARS}"));
    let applied_as = |st: &Stmt, who: &str| match st {
        Stmt::AddColumn { table, column, .. } => format!("added {table}.{column}"),
        _ => format!("applied as {who}"),
    };
    let mut wrote = false;
    // Once the admin has written, later steps are probed on the admin's session: a probe
    // may read what an earlier step added, which the service user's grants need not cover.
    let mut admin_wrote = false;
    let grant_failed = |when: &str, e: &str| (2, format!("admin-migrate: grants.sql {when}: {e} — the release ships grants the server lacks; check {ADMIN_VARS}"));
    if let Some(a) = admin {
        if let Err(e) = apply_grants(a, grants) {
            return grant_failed("before the migrations", &e);
        }
    }
    for (name, st) in steps {
        let prober: &dyn Sql = match admin {
            Some(a) if admin_wrote => a,
            _ => service,
        };
        match is_applied(prober, st) {
            Ok(true) => {
                report.push(format!("{name}: {} — skipped", what(st, true)));
                continue;
            }
            Ok(false) => {}
            Err(e) if admin_wrote => return refused(name, &e),
            Err(e) => return (2, format!("admin-migrate: {name}: cannot tell whether it is applied (read as the lifecycle service user): {e}")),
        }
        let sql = match st {
            Stmt::AddColumn { sql, .. } | Stmt::Plain(sql) => sql,
            Stmt::UnguardedAlter(_) => unreachable!("plan refuses these"),
        };
        let mut service_refused = None;
        if applier_for(grants, steps, name) == Applier::Service {
            match service.exec(sql) {
                Ok(()) => {
                    // Re-verify through the probe that called it pending.
                    match is_applied(service, st) {
                        Ok(true) => {}
                        Ok(false) => return (2, format!("admin-migrate: {name}: applied as the lifecycle service user, but its probe still finds {}", what(st, false))),
                        Err(e) => return (2, format!("admin-migrate: {name}: applied as the lifecycle service user, but cannot tell whether it took: {e}")),
                    }
                    wrote = true;
                    report.push(format!("{name}: {}", applied_as(st, "the lifecycle service user")));
                    continue;
                }
                Err(e) if is_privilege_error(&e) => service_refused = Some(e),
                Err(e) => return (2, format!("admin-migrate: {name}: pending, and applying it as the lifecycle service user failed: {e}")),
            }
        }
        let Some(admin) = admin else {
            let why = service_refused.map(|e| format!("; the lifecycle service user was refused it: {e}")).unwrap_or_default();
            return needs_admin(name, st, &why);
        };
        // The admin re-probes on its own session before writing (it sees its own writes).
        match is_applied(admin, st) {
            Ok(true) => {
                report.push(format!("{name}: {} — skipped", what(st, true)));
                continue;
            }
            Ok(false) => {}
            Err(e) => return refused(name, &e),
        }
        match admin.exec(sql) {
            Ok(()) => {
                wrote = true;
                admin_wrote = true;
                report.push(format!("{name}: {}", applied_as(st, "the database admin")));
            }
            Err(e) => return refused(name, &e),
        }
        if let Err(e) = apply_grants(admin, grants) {
            return grant_failed(&format!("after {name}"), &e);
        }
    }
    if !wrote {
        report.push(format!("admin-migrate: every migration already applied ({} step(s)) — no admin needed", steps.len()));
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

/// The admin credential spira-install provisioned: user `root`, the password in
/// `spira_config::resolve::lc_admin_password_file`. `None` when install has not written one.
pub fn provisioned_admin(env: &std::collections::BTreeMap<String, String>) -> Result<Option<(String, String)>, String> {
    let Some(path) = spira_config::resolve::lc_admin_password_file(env) else {
        return Ok(None);
    };
    let password = std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))?;
    Ok(Some(("root".to_string(), password.trim().to_string())))
}

/// The database admin as a connection: the environment's `SPIRA_LC_ADMIN_*`, else what
/// spira-install provisioned. `None` when neither names one.
pub fn admin_conn(conn: &Conn) -> Result<Option<Conn>, String> {
    let from_env = admin_from_env(|k| std::env::var(k).ok())?;
    let admin = match from_env {
        Some(a) => Some(a),
        None => provisioned_admin(&std::env::vars().collect())?,
    };
    Ok(admin.map(|(user, password)| conn.as_user(user, password)))
}

pub fn run(args: &[String], conn: &Conn) -> (i32, String) {
    let args = strip_retired_flags(args);
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
    let admin = match admin_conn(conn) {
        Ok(a) => a,
        Err(e) => return (2, format!("admin-migrate: the admin password file (SPIRA_LC_ADMIN_PASSWORD_FILE): {e}")),
    };
    // spira-install runs this with the admin as the connection's own user; the service
    // password file is not what that user authenticates with.
    let probe = match &admin {
        Some(a) if a.user == conn.user => a,
        _ => conn,
    };
    migrate(&steps, probe, admin.as_ref().map(|a| a as &dyn Sql))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_provisioned_admin_is_root_with_the_password_file_and_absent_without_one() {
        let d = testkit::TempDir::new("lc-admin-file");
        let mut env = std::collections::BTreeMap::new();
        env.insert("XDG_CONFIG_HOME".to_string(), d.path().to_string_lossy().to_string());
        assert_eq!(provisioned_admin(&env).unwrap(), None);
        std::fs::create_dir_all(d.path().join("spira")).unwrap();
        std::fs::write(d.path().join("spira/spira-lc-admin.credential"), "s3cret\n").unwrap();
        assert_eq!(provisioned_admin(&env).unwrap(), Some(("root".into(), "s3cret".into())));
    }

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
    fn the_retired_if_enforced_flag_is_ignored() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(strip_retired_flags(&a(&["--if-enforced", "m"])), a(&["m"]));
        assert_eq!(strip_retired_flags(&a(&["m"])), a(&["m"]));
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
        let mut on_disk: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).filter(|n| n.ends_with(".sql")).collect();
        on_disk.sort();
        let planned: Vec<String> = texts.iter().map(|t| t.0.clone()).collect();
        assert_eq!(planned, on_disk, "the plan is the directory, in name order");
        let numbers: Vec<u32> = planned.iter().map(|n| n.split('-').next().unwrap().parse().expect("a migration name starts with its number")).collect();
        assert_eq!(numbers, (1..=planned.len() as u32).collect::<Vec<_>>(), "migration numbers are unique and contiguous from 0001");
        let steps = plan(&texts).unwrap();
        let adds: Vec<(String, String)> = steps
            .iter()
            .filter_map(|(_, s)| match s {
                Stmt::AddColumn { table, column, .. } => Some((table.clone(), column.clone())),
                _ => None,
            })
            .collect();
        for want in [("bead", "stack"), ("bead", "stack_depth"), ("bead", "since"), ("bead", "persona"), ("bead", "title"), ("bead", "priority"), ("bead", "express"), ("batch", "pass"), ("batch", "phase"), ("bead", "aeon_phase"), ("bead", "disposition"), ("bead", "disposition_note")] {
            assert!(adds.contains(&(want.0.to_string(), want.1.to_string())), "{want:?} is added");
        }
        let views: Vec<Probe> = steps.iter().map(|(_, s)| probe_for(s)).filter(|p| matches!(p, Probe::View(_))).collect();
        for v in ["ops_live", "ops_round", "ops_recent", "ops_edges", "ops_dwell_p95", "ops_dwell"] {
            assert!(views.contains(&Probe::View(v.into())), "view {v} is probed, never always-pending");
        }
        assert!(steps.iter().any(|(_, s)| probe_for(s) == Probe::ViewColumn { view: "ops_live".into(), column: "blocker".into() }), "0011 replaces ops_live and is probed by its last alias");
        assert!(steps.iter().any(|(_, s)| probe_for(s) == Probe::ViewColumn { view: "ops_edges".into(), column: "last_at".into() }), "0013 replaces ops_edges and is probed by its last alias");
        assert!(steps.iter().all(|(_, s)| probe_for(s) != Probe::Unprobeable), "every shipped step can be probed read-only");
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
        indexes: std::collections::BTreeSet<String>,
        views: std::collections::BTreeSet<String>,
        tables: std::collections::BTreeSet<String>,
        users: std::collections::BTreeSet<String>,
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
        /// The user may run DDL (ALTER/CREATE/...) — the admin, never the service user.
        can_ddl: bool,
        /// The user may run DML (UPDATE/DELETE) — the service user's grants on `bead`.
        can_dml: bool,
        /// Every statement this user attempted to execute, applied or not.
        attempts: RefCell<Vec<String>>,
    }
    impl Sql for FakeConn {
        fn rows(&self, sql: &str) -> Result<Vec<Value>, String> {
            if self.refused {
                return Err(format!("Access denied for user '{}'", self.user));
            }
            let st = self.state.borrow();
            if let Some(view) = sql.strip_prefix("SHOW COLUMNS FROM ") {
                return Ok(st.columns.iter().filter(|(t, _)| t == view).map(|(_, c)| serde_json::json!({ "Field": c })).collect());
            }
            if sql.contains("information_schema.columns") {
                let table = sql.split("table_name = '").nth(1).and_then(|r| r.split('\'').next()).unwrap_or("");
                return Ok(st.columns.iter().filter(|(t, _)| t == table).map(|(_, c)| serde_json::json!({ "COLUMN_NAME": c })).collect());
            }
            if sql.contains("information_schema.statistics") {
                let index = sql.split("index_name = '").nth(1).and_then(|r| r.split('\'').next()).unwrap_or("");
                return Ok(if st.indexes.contains(index) { vec![serde_json::json!({ "INDEX_NAME": index })] } else { vec![] });
            }
            if sql.contains("information_schema.tables") && !sql.contains("table_type") {
                let table = sql.split("table_name = '").nth(1).and_then(|r| r.split('\'').next()).unwrap_or("");
                return Ok(if st.tables.contains(table) { vec![serde_json::json!({ "TABLE_NAME": table })] } else { vec![] });
            }
            if sql.contains("table_type = 'VIEW'") {
                let view = sql.split("table_name = '").nth(1).and_then(|r| r.split('\'').next()).unwrap_or("");
                return Ok(if st.views.contains(view) { vec![serde_json::json!({ "TABLE_NAME": view })] } else { vec![] });
            }
            if sql.contains("mysql.user") {
                let user = sql.split("user = '").nth(1).and_then(|r| r.split('\'').next()).unwrap_or("");
                return Ok(if st.users.contains(user) { vec![serde_json::json!({ "User": user })] } else { vec![] });
            }
            if sql.starts_with("SELECT 1 FROM bead WHERE") {
                return Ok(if st.stale_terminal_rows { vec![serde_json::json!({ "1": "1" })] } else { vec![] });
            }
            if sql.starts_with("SELECT 1 FROM event WHERE") {
                return Ok(vec![]);
            }
            Err(format!("fake: unexpected read {sql}"))
        }
        fn exec(&self, sql: &str) -> Result<(), String> {
            self.attempts.borrow_mut().push(sql.to_string());
            if self.refused {
                return Err(format!("Access denied for user '{}'", self.user));
            }
            let ddl = matches!(sql.split_whitespace().next().map(|t| t.to_ascii_uppercase()).as_deref(), Some("ALTER" | "CREATE" | "DROP" | "GRANT"));
            if (ddl && !self.can_ddl) || (!ddl && !self.can_dml) {
                let verb = sql.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
                return Err(format!("{verb} command denied to user '{}'@'%' for table 'bead'", self.user));
            }
            let mut st = self.state.borrow_mut();
            match classify(sql) {
                Stmt::AddColumn { table, column, .. } => {
                    if !st.columns.insert((table, column)) {
                        return Err("duplicate column".into());
                    }
                }
                Stmt::Plain(p) if p.to_ascii_uppercase().starts_with("CREATE INDEX") => {
                    st.indexes.insert(p.split_whitespace().nth(2).unwrap_or("").to_string());
                }
                Stmt::Plain(p) if p.to_ascii_uppercase().starts_with("CREATE TABLE") => {
                    st.tables.insert(p.split_whitespace().nth(5).unwrap_or("").to_string());
                }
                Stmt::Plain(p) if p.to_ascii_uppercase().starts_with("CREATE OR REPLACE VIEW") => {
                    if let Probe::ViewColumn { view, column } = probe_for(&Stmt::Plain(p.clone())) {
                        st.columns.insert((view, column));
                    }
                }
                Stmt::Plain(p) if p.to_ascii_uppercase().starts_with("CREATE VIEW") => {
                    st.views.insert(p.split_whitespace().nth(2).unwrap_or("").to_string());
                }
                Stmt::Plain(p) if p.to_ascii_uppercase().starts_with("GRANT") => {}
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
        for c in ["id", "state", "holder", "stack", "stack_depth", "since", "persona", "title", "priority", "express", "aeon_phase", "disposition", "disposition_note"] {
            st.columns.insert(("bead".into(), c.into()));
        }
        for c in ["batch_id", "state", "pass", "phase"] {
            st.columns.insert(("batch".into(), c.into()));
        }
        for i in ["event_since_idx", "bead_state_since_idx", "batch_state_idx", "event_history_idx", "event_at_idx", "idx_batch_opened"] {
            st.indexes.insert(i.into());
        }
        for v in ["ops_live", "ops_round", "ops_recent", "ops_edges", "ops_dwell_p95", "ops_dwell"] {
            st.views.insert(v.into());
        }
        st.tables.insert("bead_dep".into());
        st.users.insert("spira_lc".into());
        st.users.insert("spira_lc_ro".into());
        st.indexes.insert("bead_dep_target_idx".into());
        st.columns.insert(("ops_live".into(), "blocker".into()));
        st.columns.insert(("ops_edges".into(), "last_at".into()));
        Rc::new(RefCell::new(st))
    }

    /// The lifecycle service user as grants.sql makes it: DML on `bead`, no DDL.
    fn service(state: &Rc<RefCell<State>>) -> FakeConn {
        FakeConn { user: "spira_lc", state: state.clone(), refused: false, can_ddl: false, can_dml: true, attempts: RefCell::default() }
    }

    fn admin(state: &Rc<RefCell<State>>, refused: bool) -> FakeConn {
        FakeConn { user: "root", state: state.clone(), refused, can_ddl: true, can_dml: true, attempts: RefCell::default() }
    }

    #[test]
    fn the_admin_applies_a_missing_grant_on_an_existing_table_and_skips_one_not_yet_created() {
        let db = migrated();
        let adm = admin(&db, false);
        let grants = "GRANT SELECT ON spira_lifecycle.bead_dep TO 'spira_lc'@'%';\nGRANT SELECT ON spira_lifecycle.later TO 'spira_lc'@'%';\nCREATE USER x;\n";
        apply_grants(&adm, grants).unwrap();
        assert_eq!(adm.attempts.borrow().as_slice(), ["GRANT SELECT ON spira_lifecycle.bead_dep TO 'spira_lc'@'%'"]);
        let svc = admin(&db, false);
        let bad = FakeConn { can_ddl: false, ..svc };
        let e = apply_grants(&bad, grants).unwrap_err();
        assert!(e.contains("GRANT SELECT ON spira_lifecycle.bead_dep"), "{e}");
    }

    #[test]
    fn a_grant_to_a_user_the_server_lacks_is_skipped() {
        let db = migrated();
        db.borrow_mut().users.clear();
        let adm = admin(&db, false);
        apply_grants(&adm, "GRANT SELECT ON spira_lifecycle.bead_dep TO 'spira_lc'@'%';\n").unwrap();
        assert!(adm.attempts.borrow().is_empty());
    }

    #[test]
    fn migrate_grants_before_probing_so_a_table_added_by_an_earlier_release_is_readable() {
        let db = migrated();
        let adm = admin(&db, false);
        let (rc, out) = migrate(&real_steps(), &service(&db), Some(&adm));
        assert_eq!(rc, 0, "{out}");
        assert!(adm.attempts.borrow().iter().any(|s| s.contains("ON spira_lifecycle.bead_dep TO 'spira_lc'@'%'")), "{:?}", adm.attempts.borrow());
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
    fn a_pending_guarded_update_is_applied_by_the_service_user_with_no_admin() {
        let db = migrated();
        db.borrow_mut().stale_terminal_rows = true;
        let (rc, out) = migrate(&real_steps(), &service(&db), None);
        assert_eq!(rc, 0, "{out}");
        assert!(out.contains("0003-terminal-holder.sql: applied as the lifecycle service user"), "{out}");
        let w = db.borrow().writes.clone();
        assert_eq!(w.len(), 1, "0003 only: {w:?}");
        assert!(w[0].starts_with("spira_lc: UPDATE bead"), "{w:?}");
        // Re-verified, and a second run finds nothing pending.
        let (rc, out) = migrate(&real_steps(), &service(&db), None);
        assert_eq!(rc, 0, "{out}");
        assert!(out.contains("every migration already applied"), "{out}");
        assert_eq!(db.borrow().writes.len(), 1, "nothing re-run");
    }

    #[test]
    fn a_pending_alter_with_no_admin_is_never_tried_as_the_service_user() {
        let db = migrated();
        db.borrow_mut().columns.remove(&("bead".into(), "since".into()));
        let svc = service(&db);
        let (rc, out) = migrate(&real_steps(), &svc, None);
        assert_eq!(rc, 2, "{out}");
        assert!(out.contains("SPIRA_LC_ADMIN_USER") && out.contains("SPIRA_LC_ADMIN_PASSWORD"), "{out}");
        assert!(out.contains("0002-since.sql is pending"), "{out}");
        assert!(svc.attempts.borrow().is_empty(), "DDL is the admin's: {:?}", svc.attempts.borrow());
    }

    #[test]
    fn a_service_user_refused_on_the_update_falls_back_to_the_admin_refusal() {
        let db = migrated();
        db.borrow_mut().stale_terminal_rows = true;
        let svc = FakeConn { can_dml: false, ..service(&db) };
        let (rc, out) = migrate(&real_steps(), &svc, None);
        assert_eq!(rc, 2, "{out}");
        assert!(out.contains("0003-terminal-holder.sql is pending"), "{out}");
        assert!(out.contains("set SPIRA_LC_ADMIN_USER and SPIRA_LC_ADMIN_PASSWORD"), "{out}");
        assert_eq!(svc.attempts.borrow().len(), 1, "the service user did try: {:?}", svc.attempts.borrow());
        assert!(db.borrow().writes.is_empty());
        // With an admin configured, the same refusal is applied as the admin instead.
        let root = admin(&db, false);
        let (rc, out) = migrate(&real_steps(), &svc, Some(&root));
        assert_eq!(rc, 0, "{out}");
        assert!(db.borrow().writes.iter().all(|w| w.starts_with("root: ")), "{:?}", db.borrow().writes);
    }

    #[test]
    fn a_pending_migration_is_applied_as_the_admin() {
        let db = migrated();
        db.borrow_mut().columns.remove(&("bead".into(), "since".into()));
        db.borrow_mut().stale_terminal_rows = true;
        let admin = admin(&db, false);
        let (rc, out) = migrate(&real_steps(), &service(&db), Some(&admin));
        assert_eq!(rc, 0, "{out}");
        assert!(out.contains("0002-since.sql: added bead.since"), "{out}");
        let w: Vec<String> = db.borrow().writes.iter().filter(|w| !w.contains(": GRANT ")).cloned().collect();
        assert_eq!(w.len(), 2, "0002 and 0003 only, 0001 untouched: {w:?}");
        assert!(w[0].starts_with("root: ALTER"), "the DDL as the admin: {w:?}");
        assert!(w[1].starts_with("spira_lc: UPDATE"), "the guarded DML as the service user: {w:?}");
        // And the next activation needs no admin.
        let (rc, out) = migrate(&real_steps(), &service(&db), None);
        assert_eq!(rc, 0, "{out}");
    }

    #[test]
    fn a_refused_admin_names_the_variables_and_never_a_password() {
        let db = migrated();
        db.borrow_mut().columns.remove(&("bead".into(), "since".into()));
        let admin = admin(&db, true);
        let (rc, out) = migrate(&real_steps(), &service(&db), Some(&admin));
        assert_eq!(rc, 2, "{out}");
        assert!(out.contains("SPIRA_LC_ADMIN_USER") && out.contains("SPIRA_LC_ADMIN_PASSWORD"), "{out}");
        assert!(out.contains("grants.sql"), "{out}");
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
        assert_eq!(probe_for(&Stmt::Plain("CREATE VIEW v1 AS SELECT 1".into())), Probe::View("v1".into()));
        assert_eq!(
            probe_for(&Stmt::Plain("CREATE OR REPLACE VIEW v1 AS SELECT a AS x, (SELECT 1 FROM t) AS y FROM u JOIN (SELECT 2 AS z) q".into())),
            Probe::ViewColumn { view: "v1".into(), column: "y".into() }
        );
        assert_eq!(probe_for(&Stmt::Plain("CREATE UNIQUE INDEX i ON bead (state)".into())), Probe::Index { table: "bead".into(), index: "i".into() });
        assert_eq!(probe_for(&Stmt::Plain("INSERT INTO t VALUES (1)".into())), Probe::Unprobeable);
    }

    #[test]
    fn the_service_users_writes_are_read_from_the_shipped_grants() {
        assert!(service_may(GRANTS_SQL, "UPDATE", "bead"));
        assert!(service_may(GRANTS_SQL, "update", "batch_member"));
        assert!(!service_may(GRANTS_SQL, "UPDATE", "event"), "the event log is INSERT-only");
        assert!(service_may(GRANTS_SQL, "INSERT", "event"));
        assert!(!service_may(GRANTS_SQL, "DELETE", "bead"), "no DELETE grant anywhere");
        assert!(!service_may("GRANT UPDATE ON spira_lifecycle.bead TO 'spira_lc_ro'@'%';", "UPDATE", "bead"), "another grantee's grant is not the service user's");
    }

    #[test]
    fn only_guarded_dml_on_a_table_the_service_user_writes_is_the_service_users() {
        let g = "GRANT SELECT, INSERT, UPDATE ON spira_lifecycle.bead TO 'spira_lc'@'%';\nGRANT SELECT, DELETE ON spira_lifecycle.`batch` TO 'spira_lc'@'%';\n";
        let ok = |sql: &str| service_can_apply(g, &classify(sql));
        assert!(ok("UPDATE bead SET holder = NULL WHERE state = 'LANDED'"));
        assert!(ok("DELETE FROM batch WHERE id = 'x'"));
        assert!(!ok("UPDATE bead SET holder = NULL"), "unguarded: no WHERE");
        assert!(!ok("DELETE FROM bead WHERE id = 'x'"), "no DELETE grant on bead");
        assert!(!ok("UPDATE event SET kind = 'x' WHERE id = 1"), "no UPDATE grant on event");
        assert!(!ok("UPDATE bead, batch SET bead.holder = NULL WHERE 1 = 1"), "multi-table");
        for ddl in ["ALTER TABLE bead ADD COLUMN x INT", "CREATE TABLE t (a INT)", "DROP TABLE bead", "GRANT UPDATE ON spira_lifecycle.bead TO 'x'@'%'", "INSERT INTO bead VALUES (1)"] {
            assert!(!ok(ddl), "{ddl}");
        }
        // Per file: one DDL statement makes the whole migration the admin's.
        let steps = plan(&[("0001.sql".into(), "UPDATE bead SET a = 1 WHERE b = 2;".into()), ("0002.sql".into(), "UPDATE bead SET a = 1 WHERE b = 2; ALTER TABLE bead ADD COLUMN c INT;".into())]).unwrap();
        assert_eq!(applier_for(g, &steps, "0001.sql"), Applier::Service);
        assert_eq!(applier_for(g, &steps, "0002.sql"), Applier::Admin);
        // The shipped migrations: 0003 is the service user's, the column adds the admin's.
        let real = real_steps();
        assert_eq!(applier_for(GRANTS_SQL, &real, "0003-terminal-holder.sql"), Applier::Service);
        assert_eq!(applier_for(GRANTS_SQL, &real, "0001-stack.sql"), Applier::Admin);
        assert_eq!(applier_for(GRANTS_SQL, &real, "0002-since.sql"), Applier::Admin);
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
