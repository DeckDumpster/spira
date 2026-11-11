//! The content door's reads: plain SQL against the beads database, never a `bd` process.
//! Every read verb is one query (or the few that fill a row's labels and edges); filters are
//! WHERE clauses and nothing is selected or filtered here.
//!
//! The rows are this server's own shape, [`Bead`]: one typed row per bead, labels and
//! dependency edges as arrays. It is not `bd`'s JSON and is not kept equal to it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Map, Value};

use crate::db::Conn;
use crate::rows::escape;

const RFC3339: &str = "%Y-%m-%dT%H:%i:%sZ";
/// A list without `--limit` stops here, as `bd list` does.
const DEFAULT_LIMIT: u64 = 50;
const ID_CHUNK: usize = 500;
/// The prefix `bd remember` stores a memory under in the config table.
/// How long a refused layout stands before the store is asked again.
const LAYOUT_RECHECK: std::time::Duration = std::time::Duration::from_secs(30);
const MEMORY_PREFIX: &str = "kv.memory.";
/// Issue types `bd list` hides unless asked for by type or id.
const HIDDEN_TYPES: &[&str] = &["gate", "agent", "role", "message"];

/// The text columns a `--brief` row leaves out.
const TEXT: &[&str] = &["description", "design", "acceptance_criteria", "notes"];

/// Why a read was not answered: the request is not one the door serves (or names a bead that is
/// not there), or the store could not be asked or is not laid out as the reads expect.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    Request(String),
    Store(String),
}

impl Failure {
    pub fn text(&self) -> &str {
        match self {
            Failure::Request(t) | Failure::Store(t) => t,
        }
    }
}

type Res<T> = Result<T, Failure>;

/// Where a query is sent. The live one is a read-only connection to the beads database.
pub trait Sql {
    fn rows(&self, sql: &str) -> Res<Vec<Value>>;
}

impl Sql for Conn {
    fn rows(&self, sql: &str) -> Res<Vec<Value>> {
        self.query(sql).map_err(|e| Failure::Store(format!("{e:?}")))
    }
}

/// One edge out of a bead: it depends on (or, for `parent-child`, is a child of) `depends_on_id`.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub issue_id: String,
    pub depends_on_id: String,
    pub kind: String,
}

/// The row a content read returns for one bead.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Bead {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: i64,
    pub issue_type: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub assignee: Option<String>,
    pub owner: Option<String>,
    pub created_by: Option<String>,
    pub external_ref: Option<String>,
    pub defer_until: Option<String>,
    pub close_reason: Option<String>,
    pub await_type: Option<String>,
    pub await_id: Option<String>,
    /// description, design, acceptance_criteria, notes; empty under `--brief`.
    pub text: BTreeMap<&'static str, String>,
    pub metadata: Option<Value>,
    pub labels: Vec<String>,
    pub parent: Option<String>,
    pub dependencies: Vec<Edge>,
    pub comment_count: u64,
}

impl Bead {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        let mut put = |k: &str, v: Value| {
            m.insert(k.to_string(), v);
        };
        put("id", json!(self.id));
        put("title", json!(self.title));
        for col in TEXT {
            if let Some(t) = self.text.get(col).filter(|t| !t.is_empty()) {
                put(col, json!(t));
            }
        }
        put("status", json!(self.status));
        put("priority", json!(self.priority));
        put("issue_type", json!(self.issue_type));
        for (k, v) in [("assignee", &self.assignee), ("owner", &self.owner), ("created_by", &self.created_by), ("external_ref", &self.external_ref), ("close_reason", &self.close_reason), ("await_type", &self.await_type), ("await_id", &self.await_id)] {
            if let Some(v) = v {
                put(k, json!(v));
            }
        }
        put("created_at", json!(self.created_at));
        put("updated_at", json!(self.updated_at));
        for (k, v) in [("closed_at", &self.closed_at), ("defer_until", &self.defer_until)] {
            if let Some(v) = v {
                put(k, json!(v));
            }
        }
        if let Some(md) = &self.metadata {
            put("metadata", md.clone());
        }
        put("labels", json!(self.labels));
        put("parent", json!(self.parent));
        put("comment_count", json!(self.comment_count));
        put("dependencies", Value::Array(self.dependencies.iter().map(|e| json!({ "issue_id": e.issue_id, "depends_on_id": e.depends_on_id, "type": e.kind })).collect()));
        Value::Object(m)
    }
}

/// A parsed `content list`: each field is a WHERE clause or the LIMIT, nothing more.
#[derive(Debug, Default, PartialEq)]
pub struct List {
    pub all: bool,
    pub brief: bool,
    /// `None` is unlimited (`--limit 0`).
    pub limit: Option<u64>,
    pub ids: Vec<String>,
    pub statuses: Vec<String>,
    pub types: Vec<String>,
    pub exclude_types: Vec<String>,
    pub labels_all: Vec<String>,
    pub labels_any: Vec<String>,
    pub labels_none: Vec<String>,
    pub parent: Option<String>,
    pub no_parent: bool,
    pub external_ref: Option<String>,
    pub external_contains: Option<String>,
    pub title_contains: Option<String>,
    pub priority: Option<i64>,
    pub times: Vec<(&'static str, &'static str, String)>,
}

#[derive(Debug, PartialEq)]
pub enum Read {
    List(Box<List>),
    Show(Vec<String>),
    Comments(String),
    Gates { all: bool },
    Memories,
    State { id: String, dimension: String },
    ConfigGet(String),
}

fn csv(v: &str) -> impl Iterator<Item = String> + '_ {
    v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// `YYYY-MM-DD` or RFC 3339 in UTC, as a DATETIME literal body.
fn instant(flag: &str, v: &str) -> Result<String, String> {
    let bad = || format!("{flag} takes YYYY-MM-DD or an RFC3339 UTC time, not {v:?}");
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    let (date, rest) = match v.split_once('T') {
        Some((d, r)) => (d, Some(r)),
        None => (v, None),
    };
    let mut d = date.split('-');
    let (y, mo, da) = (d.next().unwrap_or(""), d.next().unwrap_or(""), d.next().unwrap_or(""));
    if !(digits(y, 4) && digits(mo, 2) && digits(da, 2) && d.next().is_none()) {
        return Err(bad());
    }
    let time = match rest {
        None => "00:00:00".to_string(),
        Some(r) => {
            let r = r.strip_suffix('Z').ok_or_else(bad)?;
            let r = r.split('.').next().unwrap_or("");
            let mut t = r.split(':');
            let (h, mi, s) = (t.next().unwrap_or(""), t.next().unwrap_or(""), t.next().unwrap_or(""));
            if !(digits(h, 2) && digits(mi, 2) && digits(s, 2) && t.next().is_none()) {
                return Err(bad());
            }
            r.to_string()
        }
    };
    Ok(format!("{date} {time}"))
}

/// The flag and its value, from `--flag value` or `--flag=value`.
fn take<'a>(a: &'a [String], i: &mut usize) -> Result<(&'a str, String), String> {
    let arg = a[*i].as_str();
    if let Some((f, v)) = arg.split_once('=').filter(|(f, _)| f.starts_with("--")) {
        return Ok((f, v.to_string()));
    }
    *i += 1;
    match a.get(*i) {
        Some(v) => Ok((arg, v.clone())),
        None => Err(format!("{arg} needs a value")),
    }
}

fn parse_list(a: &[String]) -> Result<List, String> {
    let mut l = List { limit: Some(DEFAULT_LIMIT), ..List::default() };
    let mut i = 0;
    while i < a.len() {
        let flag = a[i].split_once('=').filter(|(f, _)| f.starts_with("--")).map_or(a[i].as_str(), |(f, _)| f);
        match flag {
            "--all" => l.all = true,
            "--brief" => l.brief = true,
            "--json" | "--long" | "--flat" | "--no-pager" | "--tree" | "-q" | "--quiet" => {}
            "--no-parent" => l.no_parent = true,
            _ => {
                let (f, v) = take(a, &mut i)?;
                match f {
                    "--limit" | "-n" => {
                        let n: u64 = v.parse().map_err(|_| format!("--limit takes a count, not {v:?}"))?;
                        l.limit = (n != 0).then_some(n);
                    }
                    "--id" => l.ids.extend(csv(&v)),
                    "--status" | "-s" => l.statuses.extend(csv(&v)),
                    "--type" | "-t" => l.types.extend(csv(&v)),
                    "--exclude-type" => l.exclude_types.extend(csv(&v)),
                    "--label" | "-l" => l.labels_all.extend(csv(&v)),
                    "--label-any" => l.labels_any.extend(csv(&v)),
                    "--exclude-label" => l.labels_none.extend(csv(&v)),
                    "--parent" => l.parent = Some(v),
                    "--external-ref" => l.external_ref = Some(v),
                    "--external-contains" => l.external_contains = Some(v),
                    "--title" | "--title-contains" => l.title_contains = Some(v),
                    "--priority" | "-p" => l.priority = Some(v.trim_start_matches(['P', 'p']).parse().map_err(|_| format!("--priority takes 0-4 or P0-P4, not {v:?}"))?),
                    "--closed-after" => l.times.push(("closed_at", ">", instant(f, &v)?)),
                    "--closed-before" => l.times.push(("closed_at", "<", instant(f, &v)?)),
                    "--updated-after" => l.times.push(("updated_at", ">", instant(f, &v)?)),
                    "--updated-before" => l.times.push(("updated_at", "<", instant(f, &v)?)),
                    "--created-after" => l.times.push(("created_at", ">", instant(f, &v)?)),
                    "--created-before" => l.times.push(("created_at", "<", instant(f, &v)?)),
                    other => return Err(format!("list takes no `{other}`: the content door reads the filters it serves as SQL")),
                }
            }
        }
        i += 1;
    }
    Ok(l)
}

/// The content args (after `content`) of a read verb, parsed.
pub fn parse(args: &[String]) -> Result<Read, String> {
    let words = |a: &[String]| -> Vec<String> { a.iter().filter(|x| !matches!(x.as_str(), "--json" | "--long" | "-q" | "--quiet" | "--no-pager")).cloned().collect() };
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["list", ..] => parse_list(&args[1..]).map(|l| Read::List(Box::new(l))),
        ["show", ..] => {
            let ids = words(&args[1..]);
            match ids.iter().find(|i| i.starts_with('-')) {
                Some(f) => Err(format!("show takes bead ids, not `{f}`")),
                None if ids.is_empty() => Err("show needs a bead id".into()),
                None => Ok(Read::Show(ids)),
            }
        }
        ["comments", ..] => match words(&args[1..]).as_slice() {
            [id] if !id.starts_with('-') => Ok(Read::Comments(id.clone())),
            _ => Err("comments takes one bead id".into()),
        },
        ["gate", "list", rest @ ..] => {
            let rest = words(&rest.iter().map(|s| s.to_string()).collect::<Vec<_>>());
            match rest.iter().find(|f| f.as_str() != "--all") {
                Some(f) => Err(format!("gate list takes only --all, not `{f}`")),
                None => Ok(Read::Gates { all: !rest.is_empty() }),
            }
        }
        ["memories", rest @ ..] => match rest.iter().find(|f| **f != "--json") {
            Some(f) => Err(format!("memories takes no `{f}`")),
            None => Ok(Read::Memories),
        },
        ["state", ..] => match words(&args[1..]).as_slice() {
            [id, dim] if !id.starts_with('-') && !dim.starts_with('-') => Ok(Read::State { id: id.clone(), dimension: dim.clone() }),
            _ => Err("state takes a bead id and a dimension".into()),
        },
        ["config", "get", key] if !key.starts_with('-') => Ok(Read::ConfigGet(key.to_string())),
        _ => Err(format!("`{}` is not a content read", a.join(" "))),
    }
}

fn quoted(items: &[String]) -> String {
    items.iter().map(|i| format!("'{}'", escape(i))).collect::<Vec<_>>().join(",")
}

fn issue_columns() -> String {
    format!(
        "id, title, description, design, acceptance_criteria, notes, status, priority, issue_type, assignee, owner, created_by, external_ref, close_reason, await_type, await_id, metadata, \
         DATE_FORMAT(created_at, '{RFC3339}') AS created_at, DATE_FORMAT(updated_at, '{RFC3339}') AS updated_at, DATE_FORMAT(closed_at, '{RFC3339}') AS closed_at, DATE_FORMAT(defer_until, '{RFC3339}') AS defer_until"
    )
}

impl List {
    pub fn where_clause(&self, db: &str) -> String {
        let mut w: Vec<String> = Vec::new();
        if !self.ids.is_empty() {
            w.push(format!("id IN ({})", quoted(&self.ids)));
        }
        if !self.statuses.is_empty() {
            w.push(format!("status IN ({})", quoted(&self.statuses)));
        } else if !self.all {
            w.push("status <> 'closed'".into());
        }
        if !self.types.is_empty() {
            w.push(format!("issue_type IN ({})", quoted(&self.types)));
        } else if self.ids.is_empty() {
            let hidden: Vec<String> = HIDDEN_TYPES.iter().map(|t| t.to_string()).collect();
            w.push(format!("issue_type NOT IN ({})", quoted(&hidden)));
        }
        if !self.exclude_types.is_empty() {
            w.push(format!("issue_type NOT IN ({})", quoted(&self.exclude_types)));
        }
        w.push("COALESCE(is_template, 0) = 0".into());
        for l in &self.labels_all {
            w.push(format!("id IN (SELECT issue_id FROM {db}.labels WHERE label = '{}')", escape(l)));
        }
        if !self.labels_any.is_empty() {
            w.push(format!("id IN (SELECT issue_id FROM {db}.labels WHERE label IN ({}))", quoted(&self.labels_any)));
        }
        if !self.labels_none.is_empty() {
            w.push(format!("id NOT IN (SELECT issue_id FROM {db}.labels WHERE label IN ({}))", quoted(&self.labels_none)));
        }
        if let Some(p) = &self.parent {
            w.push(format!("id IN (SELECT issue_id FROM {db}.dependencies WHERE type = 'parent-child' AND depends_on_issue_id = '{}')", escape(p)));
        }
        if self.no_parent {
            w.push(format!("id NOT IN (SELECT issue_id FROM {db}.dependencies WHERE type = 'parent-child')"));
        }
        if let Some(x) = &self.external_ref {
            w.push(format!("external_ref = '{}'", escape(x)));
        }
        if let Some(x) = &self.external_contains {
            w.push(format!("LOCATE(LOWER('{}'), LOWER(external_ref)) > 0", escape(x)));
        }
        if let Some(x) = &self.title_contains {
            w.push(format!("LOCATE(LOWER('{}'), LOWER(title)) > 0", escape(x)));
        }
        if let Some(p) = self.priority {
            w.push(format!("priority = {p}"));
        }
        for (col, op, at) in &self.times {
            w.push(format!("{col} {op} '{}'", escape(at)));
        }
        w.join(" AND ")
    }

    pub fn issues_sql(&self, db: &str) -> String {
        let limit = self.limit.map_or(String::new(), |n| format!(" LIMIT {n}"));
        format!("SELECT {} FROM {db}.issues WHERE {} ORDER BY priority ASC, created_at DESC, id ASC{limit}", issue_columns(), self.where_clause(db))
    }
}

fn cell<'a>(row: &'a Value, col: &str) -> Option<&'a str> {
    row.get(col).and_then(Value::as_str)
}

fn present(row: &Value, col: &str) -> Option<String> {
    cell(row, col).filter(|v| !v.is_empty()).map(str::to_string)
}

fn bead_of(row: &Value, brief: bool) -> Option<Bead> {
    let id = present(row, "id")?;
    let mut text = BTreeMap::new();
    if !brief {
        for col in TEXT {
            text.insert(*col, cell(row, col).unwrap_or("").to_string());
        }
    }
    Some(Bead {
        id,
        title: cell(row, "title").unwrap_or("").to_string(),
        status: cell(row, "status").unwrap_or("").to_string(),
        priority: cell(row, "priority").and_then(|p| p.parse().ok()).unwrap_or(2),
        issue_type: cell(row, "issue_type").unwrap_or("").to_string(),
        created_at: cell(row, "created_at").unwrap_or("").to_string(),
        updated_at: cell(row, "updated_at").unwrap_or("").to_string(),
        closed_at: present(row, "closed_at"),
        assignee: present(row, "assignee"),
        owner: present(row, "owner"),
        created_by: present(row, "created_by"),
        external_ref: present(row, "external_ref"),
        defer_until: present(row, "defer_until"),
        close_reason: present(row, "close_reason"),
        await_type: present(row, "await_type"),
        await_id: present(row, "await_id"),
        text,
        metadata: if brief { None } else { cell(row, "metadata").and_then(|m| serde_json::from_str::<Value>(m).ok()).filter(|m| m.as_object().is_some_and(|o| !o.is_empty())) },
        ..Bead::default()
    })
}

/// The beads of `issues` rows with their labels and edges, in the rows' order.
fn fill(sql: &dyn Sql, db: &str, issues: &[Value], brief: bool) -> Res<Vec<Bead>> {
    let mut beads: Vec<Bead> = issues.iter().filter_map(|r| bead_of(r, brief)).collect();
    let ids: Vec<String> = beads.iter().map(|b| b.id.clone()).collect();
    let mut labels: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut edges: BTreeMap<String, Vec<Edge>> = BTreeMap::new();
    let mut comments: BTreeMap<String, u64> = BTreeMap::new();
    for chunk in ids.chunks(ID_CHUNK) {
        let list = quoted(chunk);
        for r in sql.rows(&format!("SELECT issue_id, COUNT(*) AS n FROM {db}.comments WHERE issue_id IN ({list}) GROUP BY issue_id"))? {
            if let (Some(id), Some(n)) = (present(&r, "issue_id"), cell(&r, "n").and_then(|n| n.parse().ok())) {
                comments.insert(id, n);
            }
        }
        for r in sql.rows(&format!("SELECT issue_id, label FROM {db}.labels WHERE issue_id IN ({list}) ORDER BY issue_id, label"))? {
            if let (Some(id), Some(l)) = (present(&r, "issue_id"), present(&r, "label")) {
                labels.entry(id).or_default().push(l);
            }
        }
        for r in sql.rows(&format!("SELECT issue_id, depends_on_issue_id, `type` FROM {db}.dependencies WHERE issue_id IN ({list}) AND depends_on_issue_id IS NOT NULL ORDER BY issue_id, depends_on_issue_id"))? {
            if let (Some(id), Some(on)) = (present(&r, "issue_id"), present(&r, "depends_on_issue_id")) {
                edges.entry(id.clone()).or_default().push(Edge { issue_id: id, depends_on_id: on, kind: present(&r, "type").unwrap_or_else(|| "blocks".into()) });
            }
        }
    }
    for b in &mut beads {
        b.comment_count = comments.remove(&b.id).unwrap_or(0);
        b.labels = labels.remove(&b.id).unwrap_or_default();
        b.dependencies = edges.remove(&b.id).unwrap_or_default();
        b.parent = b.dependencies.iter().find(|e| e.kind == "parent-child").map(|e| e.depends_on_id.clone());
    }
    Ok(beads)
}

fn array(beads: Vec<Bead>) -> String {
    Value::Array(beads.iter().map(Bead::to_json).collect()).to_string()
}

/// Answers one content read from `sql`.
pub fn answer(sql: &dyn Sql, db: &str, args: &[String]) -> Res<String> {
    match parse(args).map_err(Failure::Request)? {
        Read::List(l) => {
            let issues = sql.rows(&l.issues_sql(db))?;
            Ok(array(fill(sql, db, &issues, l.brief)?))
        }
        Read::Show(ids) => {
            let issues = sql.rows(&format!("SELECT {} FROM {db}.issues WHERE id IN ({}) ORDER BY id", issue_columns(), quoted(&ids)))?;
            if issues.is_empty() {
                return Err(Failure::Request(format!("no issue found matching {}", ids.iter().map(|i| format!("{i:?}")).collect::<Vec<_>>().join(", "))));
            }
            Ok(array(fill(sql, db, &issues, false)?))
        }
        Read::Comments(id) => {
            let rows = sql.rows(&format!("SELECT id, issue_id, author, text, DATE_FORMAT(created_at, '{RFC3339}') AS created_at FROM {db}.comments WHERE issue_id = '{}' ORDER BY created_at, id", escape(&id)))?;
            Ok(Value::Array(rows.iter().map(|r| json!({ "id": cell(r, "id"), "issue_id": cell(r, "issue_id"), "author": cell(r, "author"), "text": cell(r, "text"), "created_at": cell(r, "created_at") })).collect()).to_string())
        }
        Read::Gates { all } => {
            let l = List { all, types: vec!["gate".into()], limit: None, ..List::default() };
            let issues = sql.rows(&l.issues_sql(db))?;
            Ok(array(fill(sql, db, &issues, false)?))
        }
        Read::Memories => {
            let rows = sql.rows(&format!("SELECT `key`, value FROM {db}.config WHERE LEFT(`key`, {}) = '{MEMORY_PREFIX}' ORDER BY `key`", MEMORY_PREFIX.len()))?;
            let mut m = Map::new();
            for r in &rows {
                if let (Some(k), Some(v)) = (cell(r, "key"), cell(r, "value")) {
                    m.insert(k[MEMORY_PREFIX.len()..].to_string(), json!(v));
                }
            }
            Ok(Value::Object(m).to_string())
        }
        Read::State { id, dimension } => {
            let prefix = format!("{dimension}:");
            let rows = sql.rows(&format!("SELECT label FROM {db}.labels WHERE issue_id = '{}' AND LEFT(label, {}) = '{}' ORDER BY label", escape(&id), prefix.chars().count(), escape(&prefix)))?;
            Ok(match rows.first().and_then(|r| cell(r, "label")) {
                Some(l) => format!("{}\n", &l[prefix.len()..]),
                None => format!("(no {dimension} state set)\n"),
            })
        }
        Read::ConfigGet(key) => {
            let rows = sql.rows(&format!("SELECT value FROM {db}.config WHERE `key` = '{}'", escape(&key)))?;
            Ok(match rows.first().and_then(|r| cell(r, "value")) {
                Some(v) => format!("{v}\n"),
                None => format!("{key} (not set)\n"),
            })
        }
    }
}

/// The layout check: the `table.column` names the reads query that the beads database lacks.
pub fn layout_gaps(sql: &dyn Sql, db: &str) -> Res<Vec<String>> {
    let schema = db.trim_matches('`');
    let rows = sql.rows(&format!("SELECT table_name AS t, column_name AS c FROM information_schema.columns WHERE table_schema = '{}'", escape(schema)))?;
    let have: BTreeSet<(String, String)> = rows.iter().filter_map(|r| Some((cell(r, "t")?.to_lowercase(), cell(r, "c")?.to_lowercase()))).collect();
    Ok(crate::plan::missing_columns(&have))
}

fn refusal(db: &str, gaps: &[String]) -> String {
    format!(
        "the beads database {} does not have the layout the content reads query (missing {}); content reads are refused (exit 2), not served by forking bd",
        db.trim_matches('`'),
        gaps.join(", ")
    )
}

/// The read side of the beads database: a read-only connection and the layout verdict.
pub struct Reader {
    sql: Box<dyn Sql + Send + Sync>,
    db: String,
    /// The verdict on the layout and when it was reached: a refusal is asked again after
    /// `recheck`, so a repaired store is served without a restart; an unreadable server is asked again at once.
    verdict: Mutex<Option<(std::time::Instant, Option<String>)>>,
    recheck: std::time::Duration,
}

impl Reader {
    pub fn new(sql: Box<dyn Sql + Send + Sync>, db: String) -> Self {
        Reader { sql, db, verdict: Mutex::new(None), recheck: LAYOUT_RECHECK }
    }

    #[cfg(test)]
    fn rechecking_at(mut self, every: std::time::Duration) -> Self {
        self.recheck = every;
        self
    }

    /// `Ok` when the layout is known; the refusal otherwise.
    pub fn layout(&self) -> Res<()> {
        let mut held = self.verdict.lock().unwrap_or_else(|p| p.into_inner());
        let stale = held.as_ref().is_some_and(|(at, refusal)| refusal.is_some() && at.elapsed() >= self.recheck);
        if held.is_none() || stale {
            let gaps = layout_gaps(self.sql.as_ref(), &self.db)?;
            *held = Some((std::time::Instant::now(), (!gaps.is_empty()).then(|| refusal(&self.db, &gaps))));
        }
        match held.as_ref().and_then(|(_, r)| r.as_ref()) {
            Some(r) => Err(Failure::Store(r.clone())),
            None => Ok(()),
        }
    }

    pub fn read(&self, args: &[String]) -> Res<String> {
        self.layout()?;
        answer(self.sql.as_ref(), &self.db, args)
    }
}

/// The connection settings, from the config registry. The secret is read from its file, never
/// from argv, and appears in no message.
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password_file: String,
}

impl Settings {
    pub fn from_config() -> Result<Self, String> {
        let get = |k: &str| spira_config::process::cfg(k);
        let port = get("SPIRA_BEADS_SQL_PORT")?;
        let user = get("SPIRA_BEADS_SQL_USER")?;
        let file = get("SPIRA_BEADS_SQL_PASSWORD_FILE")?;
        let file = if file.trim().is_empty() { spira_config::process::ro_credential(&get("SPIRA_LC_PASSWORD_FILE")?) } else { file };
        Ok(Settings { host: get("SPIRA_BEADS_SQL_HOST")?, port: port.trim().parse().map_err(|e| format!("SPIRA_BEADS_SQL_PORT {port:?}: {e}"))?, user, password_file: file })
    }
}

static READER: OnceLock<Result<Reader, String>> = OnceLock::new();

/// This process's reader.
pub fn served() -> Result<&'static Reader, String> {
    READER
        .get_or_init(|| {
            let s = Settings::from_config()?;
            let db = crate::plan::content_db()?.to_string();
            let conn = Conn::reader(&s.host, s.port, &s.user, &s.password_file, db.trim_matches('`'))?;
            Ok(Reader::new(Box::new(conn), db))
        })
        .as_ref()
        .map_err(|e| e.clone())
}

/// Reads at server start: a layout the reads do not know is logged by name rather than found by
/// the first caller.
pub fn check_at_startup() -> Result<(), String> {
    served()?.layout().map_err(|f| f.text().to_string())
}

#[cfg(test)]
mod tests;
