//! The planning plane's read contract. `plan items|edges|children --ids a,b,…` answer from bd's
//! own tables over this connection — never bd's CLI, never git — each with the planning
//! watermark read before the rows, so a poller that sees the watermark unchanged has seen every
//! write. `plan stamp` advances the watermark after a planning write and mirrors it into bd's
//! own kv. Read-only on bd: spira_lc holds SELECT on bd's issues, labels and dependencies.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::content_sql::Sql;
use crate::rows;

/// Every bd column the reader names, by table: the conformance gate checks each exists in the
/// pinned bd, so a renamed column fails there and not as a claim check in production.
pub const READER_COLUMNS: &[(&str, &[&str])] = &[
    (
        "issues",
        &[
            "id", "title", "description", "design", "acceptance_criteria", "notes", "status", "priority", "issue_type", "assignee", "owner", "created_by", "external_ref", "close_reason", "await_type", "await_id", "metadata",
            "is_template", "defer_until", "created_at", "updated_at", "closed_at",
        ],
    ),
    ("labels", &["issue_id", "label"]),
    ("dependencies", &["issue_id", "depends_on_issue_id", "type"]),
    ("comments", &["id", "issue_id", "author", "text", "created_at"]),
    ("config", &["key", "value"]),
    ("events", &["issue_id", "event_type", "new_value", "actor", "created_at"]),
];

/// The tables every content read but `events` needs; `events` is served only to a store that has it.
pub const CORE_TABLES: &[&str] = &["issues", "labels", "dependencies", "comments", "config"];

/// The key bd's kv keeps the watermark under, so a restored bd carries its own.
pub const KV_KEY: &str = "spira.planning_watermark";

const CANNOT_TELL: i32 = 2;
/// The content store's database: the one the bead store's own `.beads/metadata.json` names
/// (`dolt_database`) under `spira.db`, resolved once per process. Production's store names
/// `spira`; a testdb fixture and a sim world name their own (`sptest_…`) on the server lc-serve
/// reaches. A store that names none is refused by name, never defaulted
/// (law-a-config-value-has-no-fallback): a guessed name reads some other database.
pub(crate) fn content_db() -> Result<&'static str, String> {
    static DB: std::sync::OnceLock<Result<String, String>> = std::sync::OnceLock::new();
    DB.get_or_init(|| spira_config::process::cfg("SPIRA_DB").and_then(|db| database_of(std::path::Path::new(db.trim()))))
        .as_deref()
        .map_err(|e| format!("cannot tell: {e}"))
}

/// The `dolt_database` that `<db>/.beads/metadata.json` names, as a quoted identifier.
pub fn database_of(db: &std::path::Path) -> Result<String, String> {
    if db.as_os_str().is_empty() {
        return Err("spira.db is empty: the content store's database is named by <spira.db>/.beads/metadata.json (dolt_database) — refusing".into());
    }
    let meta = db.join(".beads/metadata.json");
    let text = std::fs::read_to_string(&meta).map_err(|e| format!("{}: {e} — it names the content store's database (dolt_database); refusing", meta.display()))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: not JSON: {e}", meta.display()))?;
    let name = v.get("dolt_database").and_then(Value::as_str).unwrap_or("");
    if name.is_empty() {
        return Err(format!("{} names no dolt_database: the content store's database is not declared — refusing", meta.display()));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(format!("{}: dolt_database {name:?} is not a plain database name — refusing", meta.display()));
    }
    Ok(format!("`{name}`"))
}

const RFC3339: &str = "%Y-%m-%dT%H:%i:%sZ";

fn s<'a>(row: &'a Value, col: &str) -> Option<&'a str> {
    row.get(col).and_then(Value::as_str).filter(|v| !v.is_empty())
}

/// bd's `list --json` row shape, assembled from the three tables' rows. A bead's parent is
/// its `parent-child` target, as bd reports it.
pub fn assemble(issues: &[Value], labels: &[Value], deps: &[Value]) -> Vec<Value> {
    let mut by_label: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for l in labels {
        if let (Some(id), Some(label)) = (s(l, "issue_id"), s(l, "label")) {
            by_label.entry(id).or_default().push(label);
        }
    }
    let mut by_dep: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for d in deps {
        if let (Some(id), Some(on)) = (s(d, "issue_id"), s(d, "depends_on_id")) {
            by_dep.entry(id).or_default().push(json!({ "issue_id": id, "depends_on_id": on, "type": s(d, "type").unwrap_or("blocks") }));
        }
    }
    issues
        .iter()
        .filter_map(|i| {
            let id = s(i, "id")?;
            let deps = by_dep.remove(id).unwrap_or_default();
            let parent = deps.iter().find(|d| d["type"] == "parent-child").and_then(|d| d["depends_on_id"].as_str().map(str::to_string));
            Some(json!({
                "id": id,
                "title": s(i, "title"),
                "priority": s(i, "priority").and_then(|p| p.parse::<i64>().ok()),
                "parent": parent,
                "created_at": s(i, "created_at"),
                "updated_at": s(i, "updated_at"),
                "labels": by_label.get(id).cloned().unwrap_or_default(),
                "issue_type": s(i, "issue_type"),
                "status": s(i, "status"),
                "defer_until": s(i, "defer_until"),
                "dependencies": deps,
            }))
        })
        .collect()
}

/// `plan items|edges|children|inflow|repo-labels|watermark|stamp|schema …`: JSON with the watermark on every read.
pub fn cmd_plan(args: &[String]) -> (i32, String) {
    static HELD: HeldAnswers = Mutex::new((None, BTreeMap::new()));
    let answer = match args.first().map(String::as_str) {
        Some("watermark") => read_watermark().map(|w| json!({ "watermark": w })),
        Some("stamp") => stamp().map(|w| json!({ "watermark": w })),
        Some("schema") => return schema(),
        Some("items" | "edges" | "children") => read_watermark().and_then(|w| answer_with(args, bd()?, content_db()?, Some(w), Instant::now(), &HELD)),
        Some("inflow") if inflow_args(&args[1..]).is_err() => inflow_args(&args[1..]).map(|_| Value::Null),
        Some("inflow" | "repo-labels") => bd().and_then(|c| answer_with(args, c, content_db()?, read_watermark().ok(), Instant::now(), &HELD)),
        _ => Err(USAGE.into()),
    };
    match answer {
        Ok(v) => (0, v.to_string()),
        Err(e) => (CANNOT_TELL, e),
    }
}

/// How long an `inflow` answer is held: its window moves with the clock, not only with a write.
const INFLOW_HOLD: Duration = Duration::from_secs(60);

/// Most answers held at one watermark; a full hold is emptied rather than grown.
const HELD_ANSWERS: usize = 256;

/// Whole answers by joined argv, held under the planning watermark they were read at.
type HeldAnswers = Mutex<(Option<u64>, BTreeMap<String, (Value, Instant)>)>;

/// The read forms of `plan`, answered from `held` while `watermark` is where it was when the
/// same argv was last answered; only a good answer is held, and with no watermark nothing is.
fn answer_with(args: &[String], conn: &dyn Sql, cdb: &str, watermark: Option<u64>, now: Instant, held: &HeldAnswers) -> Result<Value, String> {
    let key = args.join("\u{1f}");
    let ttl = (args.first().map(String::as_str) == Some("inflow")).then_some(INFLOW_HOLD);
    let lock = || held.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(w) = watermark {
        let mut g = lock();
        if g.0 != Some(w) {
            *g = (Some(w), BTreeMap::new());
        }
        if let Some((v, at)) = g.1.get(&key) {
            if ttl.is_none_or(|t| now.saturating_duration_since(*at) < t) {
                return Ok(v.clone());
            }
        }
    }
    let rest = args.get(1..).unwrap_or_default();
    let w = json!(watermark);
    let answer = match args.first().map(String::as_str) {
        Some("items") => items_answer(rest, conn, cdb).map(|v| json!({ "watermark": w, "items": v })),
        Some("edges") => edges(rest, conn, cdb).map(|v| json!({ "watermark": w, "edges": v })),
        Some("children") => children(rest, conn, cdb).map(|v| json!({ "watermark": w, "items": v })),
        Some("inflow") => inflow_args(rest).and_then(|win| inflow(conn, cdb, win)),
        Some("repo-labels") => repo_labels(conn, cdb, flag(rest, "--valid").unwrap_or("")),
        _ => Err(USAGE.into()),
    }?;
    if let Some(w) = watermark {
        let mut g = lock();
        if g.0 == Some(w) {
            if g.1.len() >= HELD_ANSWERS {
                g.1.clear();
            }
            g.1.insert(key, (answer.clone(), now));
        }
    }
    Ok(answer)
}

fn items_answer(args: &[String], conn: &dyn Sql, cdb: &str) -> Result<Vec<Value>, String> {
    let plain = !args.iter().any(|a| a == "--full" || a == "--open");
    if plain && flag(args, "--ids").is_some() && flag(args, "--external-contains").is_none() && flag(args, "--label").is_none() && flag(args, "--type").is_none() {
        let mut rows = items_held_in(conn, cdb, &ids_of(args)?)?;
        rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        return Ok(rows);
    }
    items(args, conn, cdb)
}

fn inflow_args(args: &[String]) -> Result<u32, String> {
    flag(args, "--window-secs").and_then(|w| w.parse().ok()).ok_or_else(|| "plan inflow: --window-secs <n> is required".to_string())
}

const USAGE: &str = "usage: plan items (--ids a,b|- | --label L [--status a,b] | --external-contains S | --type T | --open) [--full] [--open] | plan edges --ids a,b|- [--dependents] | plan children --ids a,b|- [--full] [--open] | plan inflow --window-secs N | plan repo-labels --valid a,b | plan watermark | plan stamp | plan schema";

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn ids_of(args: &[String]) -> Result<Vec<&str>, String> {
    let ids = flag(args, "--ids").ok_or("--ids <a,b,…> is required")?;
    Ok(ids.split(',').map(str::trim).filter(|x| !x.is_empty()).collect())
}

fn items(args: &[String], conn: &dyn Sql, cdb: &str) -> Result<Vec<Value>, String> {
    let full = args.iter().any(|a| a == "--full");
    let open = args.iter().any(|a| a == "--open");
    match (flag(args, "--ids"), flag(args, "--external-contains"), flag(args, "--label"), flag(args, "--type")) {
        (Some(_), None, None, None) => fetch_with(conn, cdb, &ids_of(args)?, full, open),
        (None, Some(needle), None, None) => by_external_ref(conn, cdb, needle, full, open),
        (None, None, Some(label), None) => by_label(conn, cdb, label, flag(args, "--status"), full),
        (None, None, None, Some(kind)) => by_type(conn, cdb, kind, full, open),
        (None, None, None, None) if open && flag(args, "--status").is_none() => every_open(conn, cdb, full),
        _ => Err("plan items: exactly one of --ids <a,b,…>, --external-contains <text>, --label <label> [--status a,b], --type <type> or --open alone is required".into()),
    }
}

/// Raw edges with their types, out of `ids` (or, with `--dependents`, into them). Whether an
/// edge blocks is the readiness rule's to decide, not this reader's.
fn edges(args: &[String], conn: &dyn Sql, cdb: &str) -> Result<Vec<Value>, String> {
    let ids = ids_of(args)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    edges_where(conn, cdb, &ids, args.iter().any(|a| a == "--dependents"))
}

fn id_list(ids: &[&str]) -> String {
    ids.iter().map(|x| format!("'{}'", rows::escape(x))).collect::<Vec<_>>().join(",")
}

fn edges_where(conn: &dyn Sql, cdb: &str, ids: &[&str], into: bool) -> Result<Vec<Value>, String> {
    let list = id_list(ids);
    let q = |sql: String| conn.rows(&sql).map_err(|f| format!("cannot tell: {}", f.text()));
    // bd's column is depends_on_issue_id (beside depends_on_wisp_id / depends_on_external);
    // only issue-to-issue edges are planning edges between beads.
    let bd_side = if into { "depends_on_issue_id" } else { "issue_id" };
    let deps = q(format!(
        "SELECT issue_id, depends_on_issue_id AS depends_on_id, `type` FROM {cdb}.dependencies WHERE {bd_side} IN ({list}) AND depends_on_issue_id IS NOT NULL ORDER BY issue_id, depends_on_issue_id"
    ))?;
    Ok(deps
        .into_iter()
        .map(|d| json!({ "issue_id": s(&d, "issue_id"), "depends_on_id": s(&d, "depends_on_id"), "type": s(&d, "type").unwrap_or("blocks") }))
        .collect())
}

/// The beads whose parent is one of `ids`, as item rows.
fn children(args: &[String], conn: &dyn Sql, cdb: &str) -> Result<Vec<Value>, String> {
    let ids = ids_of(args)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let kids: BTreeSet<String> = edges_where(conn, cdb, &ids, true)?
        .iter()
        .filter(|e| e["type"] == "parent-child")
        .filter_map(|e| s(e, "issue_id").map(str::to_string))
        .collect();
    let kids: Vec<&str> = kids.iter().map(String::as_str).collect();
    fetch_with(conn, cdb, &kids, args.iter().any(|a| a == "--full"), args.iter().any(|a| a == "--open"))
}

/// The beads database's reader: one held connection, read-only.
pub(crate) fn bd() -> Result<&'static dyn Sql, String> {
    crate::content_sql::served().map(|r| r.sql()).map_err(|e| format!("cannot tell: the beads database: {e}"))
}

/// `plan schema`: every column [`READER_COLUMNS`] names that bd's database lacks. Exit 0 when
/// none is missing; otherwise 2, naming each as `table.column`.
fn schema() -> (i32, String) {
    let cdb = match content_db() {
        Ok(c) => c,
        Err(e) => return (CANNOT_TELL, e),
    };
    let conn = match bd() {
        Ok(c) => c,
        Err(e) => return (CANNOT_TELL, e),
    };
    let sql = format!("SELECT table_name AS t, column_name AS c FROM information_schema.columns WHERE table_schema = '{}'", rows::escape(cdb.trim_matches('`')));
    let have: BTreeSet<(String, String)> = match conn.rows(&sql) {
        Ok(rows) => rows.iter().filter_map(|r| Some((s(r, "t")?.to_lowercase(), s(r, "c")?.to_lowercase()))).collect(),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {}", e.text())),
    };
    let missing = missing_columns(&have);
    let code = if missing.is_empty() { 0 } else { CANNOT_TELL };
    (code, json!({ "database": cdb.trim_matches('`'), "missing": missing }).to_string())
}

pub fn missing_columns(have: &BTreeSet<(String, String)>) -> Vec<String> {
    READER_COLUMNS
        .iter()
        .flat_map(|(t, cols)| cols.iter().map(move |c| (*t, *c)))
        .filter(|(t, c)| !have.contains(&(t.to_string(), c.to_string())))
        .map(|(t, c)| format!("{t}.{c}"))
        .collect()
}

/// A control-plane `list` answer with each row's title and priority read from bd, where they
/// live: the lifecycle record keeps neither. Under `list --batches` the rows are rounds, and it
/// is each member that is titled. A row bd does not know, or a bd that cannot be read, leaves
/// them null.
/// Whether a control-plane answer to `args` lists records or rounds, whose rows carry
/// planning fields: `list --delivery` lists deliveries, which carry none.
pub fn joins_planning(args: &[String]) -> bool {
    args.first().is_some_and(|v| v == "list") && !args.iter().any(|a| a == "--delivery")
}

/// A control-plane answer to `args` with the planning fields its verb's rows lack.
pub fn join_answer(args: &[String], answer: (i32, String), lookup: &TitleLookup) -> (i32, String) {
    match args.first().map(String::as_str) {
        Some("list") if joins_planning(args) => join_planning(answer, lookup),
        Some("list-asks") => with_ask_titles(answer),
        Some("watch") if args.iter().any(|a| a == "--rows") => join_watch_rows(answer, lookup),
        _ => answer,
    }
}

/// [`join_answer`] over bd, through the planning cache.
pub fn with_planning_fields(args: &[String], answer: (i32, String)) -> (i32, String) {
    join_answer(args, answer, &|ids| titled(ids).map(|t| t.into_iter().collect()))
}

/// The titles and priorities of `ids` bd has, held under the planning watermark.
pub fn titled(ids: &[&str]) -> Result<BTreeMap<String, Titled>, String> {
    let mut out = BTreeMap::new();
    for chunk in ids.chunks(500) {
        out.extend(TITLES.titles(chunk, current_watermark(), &|missing| titles(bd()?, missing))?);
    }
    Ok(out)
}

/// The plan rows of `ids` as `plan items --ids` prints them, held under the planning watermark
/// they were read at: a row moves only by a planning write, and every one moves the watermark.
pub fn items_held(sql: &dyn Sql, ids: &[&str]) -> Result<Vec<Value>, String> {
    items_held_in(sql, content_db()?, ids)
}

fn items_held_in(sql: &dyn Sql, cdb: &str, ids: &[&str]) -> Result<Vec<Value>, String> {
    static ITEMS: HeldItems = Mutex::new((None, BTreeMap::new()));
    items_under(&ITEMS, current_watermark(), ids, &|missing| fetch_with(sql, cdb, missing, false, false))
}

type HeldItems = Mutex<(Option<u64>, BTreeMap<String, Option<Value>>)>;
type ItemLookup<'a> = dyn Fn(&[&str]) -> Result<Vec<Value>, String> + 'a;

fn items_under(held: &HeldItems, watermark: Option<u64>, ids: &[&str], fetch: &ItemLookup) -> Result<Vec<Value>, String> {
    let Some(w) = watermark else { return fetch(ids) };
    let lock = || held.lock().unwrap_or_else(|p| p.into_inner());
    let (mut out, missing) = {
        let mut g = lock();
        if g.0 != Some(w) {
            *g = (Some(w), BTreeMap::new());
        }
        let mut out = Vec::new();
        let mut missing = Vec::new();
        for id in ids {
            match g.1.get(*id) {
                Some(Some(row)) => out.push(row.clone()),
                Some(None) => {}
                None => missing.push(*id),
            }
        }
        (out, missing)
    };
    if missing.is_empty() {
        return Ok(out);
    }
    let found = fetch(&missing)?;
    let mut g = lock();
    if g.0 == Some(w) {
        for id in &missing {
            g.1.insert(id.to_string(), found.iter().find(|r| r["id"] == *id).cloned());
        }
    }
    out.extend(found);
    Ok(out)
}

/// The planning watermark as the control store's read model holds it, or `None` without one.
pub(crate) fn current_watermark() -> Option<u64> {
    let store = crate::control::served::configured()?.ok()?;
    let item = store.contract().get(crate::control::keys::PLANNING_WATERMARK).ok()??;
    item.value.data.get("seq").and_then(Value::as_u64)
}

/// Titles and priorities read from bd, kept under the planning watermark they were read at: a
/// title moves only by a planning write, and every planning write moves the watermark, so a
/// listing joins them from memory and reads bd only for ids it has not seen since the last
/// write. With no watermark nothing is kept.
#[derive(Default)]
pub struct TitleCache(Mutex<HeldTitles>);

/// The watermark the titles were read at, and each id's title, `None` for one bd does not have.
type HeldTitles = (Option<u64>, BTreeMap<String, Option<Titled>>);

static TITLES: TitleCache = TitleCache(Mutex::new((None, BTreeMap::new())));

impl TitleCache {
    /// The titles of `ids` bd has, reading through `fetch` only those not held at `watermark`.
    pub fn titles(&self, ids: &[&str], watermark: Option<u64>, fetch: &TitleLookup) -> Result<Vec<(String, Titled)>, String> {
        let Some(w) = watermark else { return fetch(ids) };
        let lock = || self.0.lock().unwrap_or_else(|p| p.into_inner());
        let (mut out, missing) = {
            let mut g = lock();
            if g.0 != Some(w) {
                *g = (Some(w), BTreeMap::new());
            }
            let mut out = Vec::new();
            let mut missing = Vec::new();
            for id in ids {
                match g.1.get(*id) {
                    Some(Some(t)) => out.push((id.to_string(), t.clone())),
                    Some(None) => {}
                    None => missing.push(*id),
                }
            }
            (out, missing)
        };
        if missing.is_empty() {
            return Ok(out);
        }
        let found = fetch(&missing)?;
        let mut g = lock();
        if g.0 == Some(w) {
            let mut known: BTreeMap<String, Option<Titled>> = missing.iter().map(|id| (id.to_string(), None)).collect();
            known.extend(found.iter().map(|(id, t)| (id.clone(), Some(t.clone()))));
            g.1.extend(known);
        }
        out.extend(found);
        Ok(out)
    }
}

/// Titles and priorities by id, for [`join_planning`].
pub type TitleLookup<'a> = dyn Fn(&[&str]) -> Result<Vec<(String, Titled)>, String> + 'a;

/// [`with_planning_fields`] over any lookup, keeping the answer's own layout (`list --batches`
/// prints pretty, `list` compact).
pub fn join_planning(answer: (i32, String), lookup: &TitleLookup) -> (i32, String) {
    let (0, text) = &answer else { return answer };
    let Ok(Value::Array(mut rows)) = serde_json::from_str::<Value>(text) else { return answer };
    let pretty = text.starts_with("[\n");
    if rows.iter().any(|r| r.get("members").is_some_and(Value::is_array)) {
        let mut members: Vec<Value> = rows.iter_mut().flat_map(|r| r["members"].as_array_mut().map(std::mem::take).unwrap_or_default()).collect();
        join_rows(&mut members, lookup);
        for r in rows.iter_mut() {
            let id = r["batch_id"].clone();
            let (mine, rest): (Vec<Value>, Vec<Value>) = members.into_iter().partition(|m| m["batch_id"] == id);
            members = rest;
            r["members"] = Value::Array(mine.into_iter().map(|mut m| {
                if let Some(o) = m.as_object_mut() {
                    o.remove("priority");
                }
                m
            }).collect());
        }
    } else {
        join_rows(&mut rows, lookup);
    }
    let out = Value::Array(rows);
    (0, if pretty { serde_json::to_string_pretty(&out).unwrap_or_default() } else { out.to_string() })
}

/// A `watch --rows` answer with its carried rows titled as a `list` row is; every other key stays.
fn join_watch_rows(answer: (i32, String), lookup: &TitleLookup) -> (i32, String) {
    let (0, text) = &answer else { return answer };
    let Ok(Value::Object(mut watch)) = serde_json::from_str::<Value>(text) else { return answer };
    let Some(Value::Array(rows)) = watch.get_mut("rows") else { return answer };
    if rows.is_empty() {
        return answer;
    }
    join_rows(rows, lookup);
    (0, Value::Object(watch).to_string())
}

fn join_rows(rows: &mut [Value], lookup: &TitleLookup) {
    let ids: Vec<String> = rows.iter().filter_map(|r| s(r, "bead_id").map(str::to_string)).collect();
    let mut planned: BTreeMap<String, Titled> = BTreeMap::new();
    for chunk in ids.chunks(500) {
        let chunk: Vec<&str> = chunk.iter().map(String::as_str).collect();
        match lookup(&chunk) {
            Ok(t) => planned.extend(t),
            Err(e) => {
                // The listing is the control plane's and stands without bd; the titles wait.
                eprintln!("spira-lc list: titles and priorities unread from bd: {e}");
                break;
            }
        }
    }
    merge_planning(rows, &planned);
}

/// A `list-asks` answer with each ask's title read from its decision bead, where it lives. A bd
/// that cannot be read leaves the titles as they were.
pub fn with_ask_titles(answer: (i32, String)) -> (i32, String) {
    let (0, text) = &answer else { return answer };
    let Ok(Value::Array(mut rows)) = serde_json::from_str::<Value>(text) else { return answer };
    let ids: Vec<&str> = rows.iter().filter_map(|r| s(r, "ask_id")).collect();
    let mut planned: BTreeMap<String, Titled> = BTreeMap::new();
    for chunk in ids.chunks(500) {
        match TITLES.titles(chunk, current_watermark(), &|missing| titles(bd()?, missing)) {
            Ok(t) => planned.extend(t),
            Err(e) => {
                eprintln!("spira-lc list-asks: titles unread from bd: {e}");
                return answer;
            }
        }
    }
    for row in rows.iter_mut() {
        if let Some((title, _)) = s(row, "ask_id").and_then(|id| planned.get(id)).cloned() {
            row["title"] = title;
        }
    }
    (0, serde_json::to_string_pretty(&Value::Array(rows)).unwrap_or_default())
}

/// A bead's title and priority, as a listing shows them.
pub type Titled = (Value, Value);

/// Title and priority of `ids`, the two fields a listing joins: one query, no labels or edges.
fn titles(conn: &dyn Sql, ids: &[&str]) -> Result<Vec<(String, Titled)>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let cdb = content_db()?;
    let rows = rows_of(conn, &format!("SELECT id, title, priority FROM {cdb}.issues WHERE id IN ({})", id_list(ids)))?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let priority = r.get("priority").and_then(|p| p.as_i64().or_else(|| p.as_str().and_then(|x| x.parse().ok())));
            Some((s(r, "id")?.to_string(), (s(r, "title").map_or(Value::Null, |t| json!(t)), priority.map_or(Value::Null, |p| json!(p)))))
        })
        .collect())
}

/// Puts each row's title and priority from `planned` just before its `repo`, where the dolt
/// path's listing has them.
fn merge_planning(rows: &mut [Value], planned: &BTreeMap<String, Titled>) {
    for row in rows.iter_mut() {
        let (title, priority) = s(row, "bead_id").and_then(|id| planned.get(id)).cloned().unwrap_or((Value::Null, Value::Null));
        let Value::Object(old) = row.take() else { continue };
        let mut o = serde_json::Map::new();
        for (k, v) in old {
            if k == "repo" {
                o.insert("title".into(), title.clone());
                o.insert("priority".into(), priority.clone());
            }
            o.insert(k, v);
        }
        o.entry("title").or_insert(title);
        o.entry("priority").or_insert(priority);
        *row = Value::Object(o);
    }
}

// ---- the watermark ------------------------------------------------------------------------

static SERVING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this process is lc-serve, whose own planning writes stamp in-process.
pub fn serving() -> bool {
    SERVING.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn serve_here() {
    SERVING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The control plane's watermark, the state store's `planning/watermark`. Never bd's kv, which
/// is the planning plane's copy for a restore.
pub fn read_watermark() -> Result<u64, String> {
    let store = crate::control::served::store().map_err(|e| format!("cannot tell: {e}"))?;
    control_seq(&store).map(|(seq, _)| seq)
}

/// The control store's sequence and the revision it was read at; `0` before the key's first write.
fn control_seq<B: state_store::Backend>(store: &crate::control::store::Store<B>) -> Result<(u64, Option<state_store::Rev>), String> {
    let key = crate::control::keys::PLANNING_WATERMARK;
    let item = store.contract().get(key).map_err(|e| format!("cannot tell: {key}: {e}"))?;
    match item {
        Some(i) => Ok((i.value.data.get("seq").and_then(Value::as_u64).unwrap_or(0), Some(i.mod_rev))),
        None => Ok((0, None)),
    }
}

const STAMP_ATTEMPTS: usize = 16;

/// Advances the watermark by one, in one transaction, and returns the value it now holds.
pub fn stamp() -> Result<u64, String> {
    let store = crate::control::served::store().map_err(|e| format!("cannot tell: {e}"))?;
    let seq = stamp_control(&store)?;
    mirror(seq);
    Ok(seq)
}

pub(crate) fn stamp_control<B: state_store::Backend>(store: &crate::control::store::Store<B>) -> Result<u64, String> {
    use state_store::{Compare, Object, Request, Txn};
    let key = crate::control::keys::PLANNING_WATERMARK;
    for _ in 0..STAMP_ATTEMPTS {
        let (seq, rev) = control_seq(store)?;
        let next = seq + 1;
        let txn = Txn::new().when(Compare::ModRev(key.into(), rev)).put(key, Object::new(json!({ "seq": next })));
        let req = Request::new(&crate::control::served::request_id(), "spira-lc", "plan-stamp");
        let done = store.contract().txn(&txn, &req).map_err(|e| format!("cannot tell: plan stamp: {e}"))?;
        if done.succeeded {
            return Ok(next);
        }
    }
    Err(format!("cannot tell: plan stamp: {key} kept moving under {STAMP_ATTEMPTS} attempts"))
}

/// Stamps after a planning write this process made itself: in-process inside lc-serve, else
/// through the service, else against the store directly. A failed stamp is reported and never
/// fails the write, which has already happened.
pub fn stamp_after_write() {
    if crate::settle::owe_stamp() {
        return;
    }
    let done = if serving() {
        stamp().map(|_| ())
    } else {
        match crate::client::try_socket(&["plan".to_string(), "stamp".to_string()]) {
            Some((0, _)) => Ok(()),
            Some((code, out)) => Err(format!("exit {code}: {out}")),
            None => stamp().map(|_| ()),
        }
    };
    if let Err(e) = done {
        eprintln!("spira-lc: the planning write landed but the watermark did not advance: {e}");
    }
}

/// The stamp lc-serve's settler thread makes.
pub fn stamp_served() -> Result<u64, String> {
    stamp()
}

/// The value bd's kv must come to hold, and the last one written there.
struct Mirror {
    want: Mutex<(u64, u64)>,
    wake: Condvar,
}

static MIRROR: OnceLock<Mirror> = OnceLock::new();

/// Inside lc-serve the kv write is the mirror thread's, off the request's deadline; elsewhere it
/// is written here. Either way bd's kv only ever moves forward.
fn mirror(seq: u64) {
    match MIRROR.get() {
        Some(m) => {
            let mut g = m.want.lock().unwrap_or_else(|e| e.into_inner());
            g.0 = g.0.max(seq);
            m.wake.notify_one();
        }
        None => {
            if let Err(e) = write_kv(seq) {
                eprintln!("spira-lc: bd kv {KV_KEY} not advanced to {seq}: {e}");
            }
        }
    }
}

/// Writes `seq` into bd's kv unless kv already holds a later one: two processes that stamped
/// can reach bd in either order, and the kv copy only moves forward.
fn write_kv(seq: u64) -> Result<(), String> {
    if let Some(done) = crate::content_write::upsert_kv(KV_KEY, seq) {
        return done;
    }
    let held = crate::skew::parse_kv(crate::bd::kv_get(KV_KEY)?.as_deref())?.unwrap_or(0);
    if held >= seq {
        return Ok(());
    }
    crate::bd::write_unstamped(&["kv", "set", KV_KEY, &seq.to_string()]).map(|_| ())
}

/// The mirror thread lc-serve runs: writes the newest watermark into bd's kv whenever it is
/// ahead of what was written, starting from the watermark the service opened at.
pub fn run_mirror() {
    let Some(m) = MIRROR.get() else { return };
    loop {
        let target = {
            let mut g = m.want.lock().unwrap_or_else(|e| e.into_inner());
            while g.0 <= g.1 {
                g = m.wake.wait(g).unwrap_or_else(|e| e.into_inner());
            }
            g.0
        };
        match write_kv(target) {
            Ok(()) => m.want.lock().unwrap_or_else(|e| e.into_inner()).1 = target,
            Err(e) => {
                eprintln!("spira-lc serve: bd kv {KV_KEY} not advanced to {target}: {e}");
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        }
    }
}

/// Installs the mirror before any request is served, so a stamp inside lc-serve never writes
/// kv on the request's own deadline; `start` is written once, healing a kv a crash left behind.
pub fn install_mirror(start: u64) {
    let _ = MIRROR.set(Mirror { want: Mutex::new((start, 0)), wake: Condvar::new() });
}

/// The item rows of `ids`.
pub fn fetch(ids: &[&str]) -> Result<Vec<Value>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    fetch_with(bd()?, content_db()?, ids, false, false)
}

/// [`fetch`] with each row's text columns, the description among them.
pub fn fetch_described(ids: &[&str]) -> Result<Vec<Value>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    fetch_with(bd()?, content_db()?, ids, true, false)
}

fn rows_of(conn: &dyn Sql, sql: &str) -> Result<Vec<Value>, String> {
    conn.rows(sql).map_err(|f| format!("cannot tell: {}", f.text()))
}

const TEXT_COLUMNS: &str = ", description, notes, external_ref, close_reason, DATE_FORMAT(closed_at, '%Y-%m-%dT%H:%i:%sZ') AS closed_at";

fn by_external_ref(conn: &dyn Sql, cdb: &str, needle: &str, full: bool, open: bool) -> Result<Vec<Value>, String> {
    if needle.is_empty() {
        return Err("plan items: --external-contains needs a non-empty text".into());
    }
    let sql = format!("SELECT id FROM {cdb}.issues WHERE LOCATE('{}', external_ref) > 0 ORDER BY id", rows::escape(needle));
    let found = rows_of(conn, &sql)?;
    let ids: Vec<&str> = found.iter().filter_map(|r| s(r, "id")).collect();
    fetch_with(conn, cdb, &ids, full, open)
}

/// The beads carrying `label`, narrowed to the comma-separated `statuses` when given.
fn by_label(conn: &dyn Sql, cdb: &str, label: &str, statuses: Option<&str>, full: bool) -> Result<Vec<Value>, String> {
    if label.is_empty() {
        return Err("plan items: --label needs a non-empty label".into());
    }
    let status = match statuses {
        None => String::new(),
        Some(list) => format!(" AND status IN ({})", list.split(',').filter(|x| !x.is_empty()).map(|x| format!("'{}'", rows::escape(x))).collect::<Vec<_>>().join(",")),
    };
    let sql = format!("SELECT id FROM {cdb}.issues WHERE id IN (SELECT issue_id FROM {cdb}.labels WHERE label = '{}'){status} ORDER BY id", rows::escape(label));
    let found = rows_of(conn, &sql)?;
    let ids: Vec<&str> = found.iter().filter_map(|r| s(r, "id")).collect();
    fetch_with(conn, cdb, &ids, full, false)
}

/// The beads of bd issue type `kind`.
fn by_type(conn: &dyn Sql, cdb: &str, kind: &str, full: bool, open: bool) -> Result<Vec<Value>, String> {
    if kind.is_empty() {
        return Err("plan items: --type needs a non-empty type".into());
    }
    let live = if open { " AND status <> 'closed'" } else { "" };
    let found = rows_of(conn, &format!("SELECT id FROM {cdb}.issues WHERE issue_type = '{}'{live} ORDER BY id", rows::escape(kind)))?;
    let ids: Vec<&str> = found.iter().filter_map(|r| s(r, "id")).collect();
    fetch_with(conn, cdb, &ids, full, open)
}

/// Every bead bd holds open.
fn every_open(conn: &dyn Sql, cdb: &str, full: bool) -> Result<Vec<Value>, String> {
    let found = rows_of(conn, &format!("SELECT id FROM {cdb}.issues WHERE status <> 'closed' ORDER BY id"))?;
    let ids: Vec<&str> = found.iter().filter_map(|r| s(r, "id")).collect();
    fetch_with(conn, cdb, &ids, full, true)
}

fn fetch_with(conn: &dyn Sql, cdb: &str, ids: &[&str], full: bool, open: bool) -> Result<Vec<Value>, String> {
    // An empty id list is an empty answer: it needs no store, so it is decided before the
    // content database is resolved (r-auto-148: an unconfigured test process was refused here).
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let list = ids.iter().map(|x| format!("'{}'", rows::escape(x))).collect::<Vec<_>>().join(",");
    let q = |sql: String| conn.rows(&sql).map_err(|f| format!("cannot tell: {}", f.text()));
    let text = if full { TEXT_COLUMNS } else { "" };
    let live = if open { " AND status <> 'closed'" } else { "" };
    let issues = q(format!(
        "SELECT id, title, priority, issue_type, status, DATE_FORMAT(defer_until, '{RFC3339}') AS defer_until, DATE_FORMAT(created_at, '{RFC3339}') AS created_at, DATE_FORMAT(updated_at, '{RFC3339}') AS updated_at{text} \
         FROM {cdb}.issues WHERE id IN ({list}){live} ORDER BY id"
    ))?;
    let labels = q(format!("SELECT issue_id, label FROM {cdb}.labels WHERE issue_id IN ({list}) ORDER BY issue_id, label"))?;
    let deps = edges_where(conn, cdb, ids, false)?;
    let mut out = assemble(&issues, &labels, &deps);
    if full {
        for (row, issue) in out.iter_mut().zip(issues.iter().filter(|i| s(i, "id").is_some())) {
            for col in ["description", "notes", "external_ref", "close_reason", "closed_at"] {
                row[col] = json!(issue.get(col).and_then(Value::as_str).unwrap_or(""));
            }
        }
    }
    Ok(out)
}

/// The pane's window on the content store: live beads plus the last day's closures.
const PANE_SCOPE: &str = "(status <> 'closed' OR closed_at >= UTC_TIMESTAMP() - INTERVAL 1 DAY)";
const INFLOW_ROWS: usize = 40;

fn count(conn: &dyn Sql, sql: String) -> Result<i64, String> {
    let rows = rows_of(conn, &sql)?;
    Ok(rows.first().and_then(|r| r.get("n")).and_then(|n| n.as_i64().or_else(|| n.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0))
}

fn inflow(conn: &dyn Sql, cdb: &str, window_secs: u32) -> Result<Value, String> {
    let q = |sql: String| rows_of(conn, &sql);
    let work = "COALESCE(issue_type, 'task') <> 'event'";
    let fresh = format!("{work} AND {PANE_SCOPE} AND created_at >= UTC_TIMESTAMP() - INTERVAL {window_secs} SECOND");
    let issues = q(format!(
        "SELECT id, title, priority, issue_type, status, DATE_FORMAT(created_at, '{RFC3339}') AS created_at FROM {cdb}.issues \
         WHERE {work} AND {PANE_SCOPE} AND created_at IS NOT NULL ORDER BY created_at DESC, id LIMIT {INFLOW_ROWS}"
    ))?;
    let ids: Vec<String> = issues.iter().filter_map(|i| s(i, "id")).map(|i| format!("'{}'", rows::escape(i))).collect();
    let labels = if ids.is_empty() {
        vec![]
    } else {
        q(format!("SELECT issue_id, label FROM {cdb}.labels WHERE issue_id IN ({}) ORDER BY issue_id, label", ids.join(",")))?
    };
    let kinds = q(format!("SELECT COALESCE(issue_type, 'task') AS kind, COUNT(*) AS n FROM {cdb}.issues WHERE {fresh} GROUP BY kind ORDER BY n DESC, kind"))?;
    let n = count(conn, format!("SELECT COUNT(*) AS n FROM {cdb}.issues WHERE {fresh}"))?;
    let defect = count(
        conn,
        format!("SELECT COUNT(*) AS n FROM {cdb}.issues WHERE {fresh} AND (issue_type = 'bug' OR id IN (SELECT issue_id FROM {cdb}.labels WHERE label = 'incident'))"),
    )?;
    let kinds: Vec<Value> = kinds.iter().map(|k| json!([s(k, "kind").unwrap_or("task"), k.get("n").and_then(|n| n.as_i64().or_else(|| n.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0)])).collect();
    Ok(json!({ "n": n, "defect": defect, "kinds": kinds, "rows": assemble(&issues, &labels, &[]) }))
}

fn repo_labels(conn: &dyn Sql, cdb: &str, valid: &str) -> Result<Value, String> {
    let live = format!("{cdb}.issues WHERE status <> 'closed'");
    let with_repo = format!("{live} AND id IN (SELECT issue_id FROM {cdb}.labels WHERE label LIKE 'repo:%')");
    let valid: Vec<String> = valid.split(',').filter(|v| !v.is_empty()).map(|v| format!("'repo:{}'", rows::escape(v))).collect();
    let mapped = if valid.is_empty() {
        0
    } else {
        count(conn, format!("SELECT COUNT(*) AS n FROM {live} AND id IN (SELECT issue_id FROM {cdb}.labels WHERE label IN ({}))", valid.join(",")))?
    };
    let total = count(conn, format!("SELECT COUNT(*) AS n FROM {live}"))?;
    let repo = count(conn, format!("SELECT COUNT(*) AS n FROM {with_repo}"))?;
    Ok(json!({ "unmapped": repo - mapped, "absent": total - repo }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: &str) -> Vec<Value> {
        serde_json::from_str(x).unwrap()
    }

    #[test_id(t3y6hiu)]
    #[test]
    fn plan_rows_are_held_under_the_watermark_and_an_absent_id_is_held_as_absent() {
        let held: HeldItems = Mutex::new((None, BTreeMap::new()));
        let asked = Mutex::new(Vec::<String>::new());
        let fetch = |ids: &[&str]| -> Result<Vec<Value>, String> {
            asked.lock().unwrap().push(ids.join(","));
            Ok(ids.iter().filter(|i| **i != "sp-gone").map(|i| json!({ "id": i, "title": "t" })).collect())
        };
        assert_eq!(items_under(&held, Some(5), &["sp-a", "sp-gone"], &fetch).unwrap().len(), 1);
        assert_eq!(items_under(&held, Some(5), &["sp-a", "sp-gone", "sp-b"], &fetch).unwrap().len(), 2);
        assert_eq!(*asked.lock().unwrap(), ["sp-a,sp-gone", "sp-b"], "only the ids not yet held, the absent one among those held");
        items_under(&held, Some(6), &["sp-a"], &fetch).unwrap();
        assert_eq!(asked.lock().unwrap().len(), 3, "a new watermark empties the hold");
        items_under(&held, None, &["sp-a"], &fetch).unwrap();
        items_under(&held, None, &["sp-a"], &fetch).unwrap();
        assert_eq!(asked.lock().unwrap().len(), 5, "no watermark holds nothing");
    }

    #[test_id(tc1z0py)]
    #[test]
    fn a_listings_titles_are_read_from_bd_once_per_planning_watermark() {
        let asked = std::cell::RefCell::new(Vec::<Vec<String>>::new());
        let bd = |ids: &[&str]| -> Result<Vec<(String, Titled)>, String> {
            asked.borrow_mut().push(ids.iter().map(|s| s.to_string()).collect());
            Ok(ids.iter().filter(|id| **id != "sp-gone").map(|id| (id.to_string(), (json!(format!("title of {id}")), json!(2)))).collect())
        };
        let cache = TitleCache::default();
        let got = cache.titles(&["sp-a", "sp-gone"], Some(5), &bd).unwrap();
        assert_eq!(got, [("sp-a".to_string(), (json!("title of sp-a"), json!(2)))]);
        let got = cache.titles(&["sp-a", "sp-gone", "sp-b"], Some(5), &bd).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(*asked.borrow(), [vec!["sp-a", "sp-gone"], vec!["sp-b"]], "only an id not yet seen at this watermark is read, and an unknown one is remembered");
        cache.titles(&["sp-a"], Some(6), &bd).unwrap();
        assert_eq!(asked.borrow().last().unwrap(), &["sp-a"], "a moved watermark reads the title again");
        cache.titles(&["sp-a"], None, &bd).unwrap();
        cache.titles(&["sp-a"], None, &bd).unwrap();
        assert_eq!(asked.borrow().len(), 5, "with no watermark every listing reads bd");
    }

    /// sp-jf7spl: the content database is the one the store declares, never a default.
    #[test_id(thdseq9)]
    #[test]
    fn the_content_database_is_the_stores_own_and_an_undeclared_one_is_refused() {
        let d = testkit::TempDir::new("content-db");
        std::fs::create_dir_all(d.join(".beads")).unwrap();
        let meta = d.join(".beads/metadata.json");
        std::fs::write(&meta, r#"{"dolt_database":"sptest_x"}"#).unwrap();
        assert_eq!(database_of(d.path()).unwrap(), "`sptest_x`");
        std::fs::write(&meta, r#"{"dolt_mode":"server"}"#).unwrap();
        let e = database_of(d.path()).unwrap_err();
        assert!(e.contains("names no dolt_database") && e.contains(&meta.display().to_string()), "{e}");
        std::fs::write(&meta, r#"{"dolt_database":"x`; DROP"}"#).unwrap();
        assert!(database_of(d.path()).is_err());
        assert!(database_of(std::path::Path::new("")).is_err());
    }

    #[test_id(tgp5goc)]
    #[test]
    fn a_row_carries_labels_blockers_and_the_parent_bd_would_report() {
        let issues = v(r#"[{"id":"sp-a","title":"t","priority":"1","issue_type":"task","status":"open","defer_until":"","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z"}]"#);
        let labels = v(r#"[{"issue_id":"sp-a","label":"plan"},{"issue_id":"sp-a","label":"spira"},{"issue_id":"sp-z","label":"stray"}]"#);
        let deps = v(r#"[{"issue_id":"sp-a","depends_on_id":"sp-b","type":"blocks"},{"issue_id":"sp-a","depends_on_id":"sp-e","type":"parent-child"}]"#);
        let out = assemble(&issues, &labels, &deps);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r["labels"], json!(["plan", "spira"]));
        assert_eq!(r["priority"], json!(1));
        assert_eq!(r["parent"], json!("sp-e"));
        assert_eq!(r["defer_until"], Value::Null);
        assert_eq!(r["dependencies"][0], json!({"issue_id":"sp-a","depends_on_id":"sp-b","type":"blocks"}));
    }

    #[test_id(trjjbsv)]
    #[test]
    fn a_bead_with_no_labels_or_edges_has_empty_lists() {
        let out = assemble(&v(r#"[{"id":"sp-a","priority":"2"}]"#), &[], &[]);
        assert_eq!((out[0]["labels"].clone(), out[0]["dependencies"].clone(), out[0]["parent"].clone()), (json!([]), json!([]), Value::Null));
    }

    const DB: &str = "`beads`";

    fn a(x: &[&str]) -> Vec<String> {
        x.iter().map(|s| s.to_string()).collect()
    }


    /// A beads database that records each query and answers by shape.
    struct Recording {
        seen: Mutex<Vec<String>>,
        fail: std::sync::atomic::AtomicBool,
    }

    impl Recording {
        fn new() -> Self {
            Recording { seen: Mutex::new(Vec::new()), fail: false.into() }
        }
        fn queries(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
    }

    impl Sql for Recording {
        fn rows(&self, sql: &str) -> Result<Vec<Value>, crate::content_sql::Failure> {
            self.seen.lock().unwrap().push(sql.to_string());
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::content_sql::Failure::Store("down".into()));
            }
            if sql.contains("COUNT(*) AS n") {
                return Ok(vec![json!({ "n": 3 })]);
            }
            if sql.contains(".labels WHERE issue_id IN") {
                return Ok(vec![json!({ "issue_id": "a", "label": "l" })]);
            }
            if sql.contains(".dependencies") {
                return Ok(vec![json!({ "issue_id": "e", "depends_on_id": "a", "type": "parent-child" })]);
            }
            Ok(vec![json!({ "id": "a", "title": "ta", "priority": 2, "issue_type": "task", "status": "open" }), json!({ "id": "b", "title": "tb", "priority": 1, "issue_type": "task", "status": "open" })])
        }
    }

    fn fresh_hold() -> HeldAnswers {
        Mutex::new((None, BTreeMap::new()))
    }

    const FORMS: &[&[&str]] = &[
        &["items", "--label", "L", "--status", "open", "--full"],
        &["items", "--type", "epic", "--open"],
        &["items", "--ids", "a,b"],
        &["edges", "--ids", "a,b", "--dependents"],
        &["children", "--ids", "e"],
        &["repo-labels", "--valid", "x,y"],
    ];

    #[test_id(tgsoh8c)]
    #[test]
    fn each_plan_form_is_queried_once_per_planning_watermark() {
        for form in FORMS {
            let (db, held, now) = (Recording::new(), fresh_hold(), Instant::now());
            let args = a(form);
            let first = answer_with(&args, &db, DB, Some(5), now, &held).unwrap();
            let once = db.queries();
            assert!(once > 0, "{form:?}");
            assert_eq!(answer_with(&args, &db, DB, Some(5), now, &held).unwrap(), first, "{form:?}");
            assert_eq!(db.queries(), once, "{form:?} was queried again at the same watermark");
            answer_with(&args, &db, DB, Some(6), now, &held).unwrap();
            assert_eq!(db.queries(), 2 * once, "{form:?} was held across a moved watermark");
        }
    }

    #[test_id(t6vy1qm)]
    #[test]
    fn an_inflow_answer_is_held_for_the_hold_and_no_longer() {
        let (db, held, t0) = (Recording::new(), fresh_hold(), Instant::now());
        let args = a(&["inflow", "--window-secs", "3600"]);
        answer_with(&args, &db, DB, Some(5), t0, &held).unwrap();
        let once = db.queries();
        answer_with(&args, &db, DB, Some(5), t0 + INFLOW_HOLD - Duration::from_secs(1), &held).unwrap();
        assert_eq!(db.queries(), once);
        answer_with(&args, &db, DB, Some(5), t0 + INFLOW_HOLD, &held).unwrap();
        assert_eq!(db.queries(), 2 * once, "past the hold it queries again");
    }

    #[test_id(t0cd46f)]
    #[test]
    fn a_failed_read_and_a_missing_watermark_are_never_held() {
        let (db, held, now) = (Recording::new(), fresh_hold(), Instant::now());
        let args = a(FORMS[3]);
        db.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(answer_with(&args, &db, DB, Some(5), now, &held).is_err());
        db.fail.store(false, std::sync::atomic::Ordering::SeqCst);
        let before = db.queries();
        answer_with(&args, &db, DB, Some(5), now, &held).unwrap();
        assert!(db.queries() > before, "the error was held");
        let before = db.queries();
        answer_with(&args, &db, DB, None, now, &held).unwrap();
        answer_with(&args, &db, DB, None, now, &held).unwrap();
        let spent = db.queries() - before;
        assert!(spent > 0 && spent % 2 == 0, "with no watermark every call queries");
    }

    #[test_id(tg01276)]
    #[test]
    fn a_held_answer_is_the_answer_the_readers_give() {
        let db = Recording::new();
        let direct = |f: &[&str]| -> Value {
            let r = a(f);
            let rest = &r[1..];
            match f[0] {
                "items" => json!({ "watermark": 5, "items": items(rest, &db, DB).unwrap() }),
                "edges" => json!({ "watermark": 5, "edges": edges(rest, &db, DB).unwrap() }),
                "children" => json!({ "watermark": 5, "items": children(rest, &db, DB).unwrap() }),
                _ => repo_labels(&db, DB, flag(rest, "--valid").unwrap()).unwrap(),
            }
        };
        for form in FORMS {
            let got = answer_with(&a(form), &db, DB, Some(5), Instant::now(), &fresh_hold()).unwrap();
            assert_eq!(got.to_string(), direct(form).to_string(), "{form:?}");
        }
    }

    #[test_id(t6asjoa)]
    #[test]
    fn plan_inflow_without_a_window_is_cannot_tell_naming_the_flag() {
        let (code, text) = cmd_plan(&a(&["inflow"]));
        assert_eq!(code, CANNOT_TELL);
        assert!(text.contains("plan inflow: --window-secs"), "{text}");
    }

    #[test_id(tkfvsmf)]
    #[test]
    fn the_usage_names_both_aggregate_sub_verbs() {
        let (_, text) = cmd_plan(&a(&["nonsense"]));
        assert!(text.contains("plan inflow --window-secs N") && text.contains("plan repo-labels --valid a,b"), "{text}");
    }

    #[test_id(t4rp98n)]
    #[test]
    fn no_ids_is_an_empty_answer_and_a_missing_selector_is_refused() {
        let c = crate::db::Conn::unreachable();
        assert_eq!(items(&a(&["--ids", ""]), &c, DB), Ok(vec![]));
        assert_eq!(children(&a(&["--ids", ","]), &c, DB), Ok(vec![]));
        assert_eq!(edges(&a(&["--ids", ""]), &c, DB), Ok(vec![]));
        assert!(items(&[], &c, DB).is_err());
        assert!(edges(&[], &c, DB).unwrap_err().contains("--ids"));
        assert_eq!(cmd_plan(&a(&["nope"])).0, CANNOT_TELL);
    }

    #[test_id(tdged6f)]
    #[test]
    fn items_open_alone_answers_the_non_closed_rows_and_a_second_selector_is_still_refused() {
        let c = Recording::new();
        let rows = items(&a(&["--open"]), &c, DB).unwrap();
        assert_eq!(rows.len(), 2);
        let seen = c.seen.lock().unwrap().clone();
        assert!(seen[0].contains("status <> 'closed'") && !seen[0].contains("issue_type ="), "{seen:?}");
        assert!(seen[1].contains("status <> 'closed'"), "the issues read stays open-only: {seen:?}");
        for second in [&["--open", "--type", "epic", "--ids", "a"][..], &["--open", "--label", "L", "--type", "epic"]] {
            let err = items(&a(second), &c, DB).unwrap_err();
            assert!(err.contains("exactly one of"), "{err}");
        }
        assert!(items(&a(&["--open", "--status", "open"]), &c, DB).unwrap_err().contains("exactly one of"));
        assert!(items(&a(&["--full"]), &c, DB).unwrap_err().contains("exactly one of"));
    }

    #[test_id(tr5m1ak)]
    #[test]
    fn the_schema_check_names_each_column_the_reader_needs_and_the_store_lacks() {
        let mut have: BTreeSet<(String, String)> = READER_COLUMNS.iter().flat_map(|(t, cs)| cs.iter().map(move |c| (t.to_string(), c.to_string()))).collect();
        assert_eq!(missing_columns(&have), Vec::<String>::new());
        have.remove(&("dependencies".to_string(), "depends_on_issue_id".to_string()));
        assert_eq!(missing_columns(&have), vec!["dependencies.depends_on_issue_id".to_string()]);
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    #[test_id(tntl3hf)]
    #[test]
    fn a_watch_with_rows_takes_the_title_and_priority_a_list_row_does() {
        let lookup: &TitleLookup = &|ids| Ok(ids.iter().filter(|i| **i == "sp-a").map(|i| (i.to_string(), (json!("the title"), json!(1)))).collect());
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let rows = json!([{"bead_id": "sp-a", "state": "READY"}, {"bead_id": "sp-gone", "state": "READY"}]);
        let watch = json!({"events": [{"rev": 3}], "through": 3, "progress_rev": 3, "rows": rows}).to_string();
        let (code, out) = join_answer(&args(&["watch", "lifecycle", "--rows"]), (0, watch), lookup);
        assert_eq!(code, 0);
        let out: Value = serde_json::from_str(&out).unwrap();
        assert_eq!((out["rows"][0]["title"].clone(), out["rows"][0]["priority"].clone()), (json!("the title"), json!(1)), "{out}");
        assert_eq!((out["rows"][1]["title"].clone(), out["rows"][1]["priority"].clone()), (Value::Null, Value::Null), "{out}");
        assert_eq!((&out["events"], &out["through"], &out["progress_rev"]), (&json!([{"rev": 3}]), &json!(3), &json!(3)));
        let (_, listed) = join_answer(&args(&["list", "--ids", "sp-a,sp-gone"]), (0, rows.to_string()), lookup);
        assert_eq!(out["rows"], serde_json::from_str::<Value>(&listed).unwrap(), "the same rows a list prints");
    }

    #[test_id(tz53xs8)]
    #[test]
    fn a_watch_without_rows_or_a_failed_one_passes_through_unchanged() {
        let lookup: &TitleLookup = &|_| panic!("no lookup for an answer without rows to join");
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let plain = r#"{"events":[],"through":3,"progress_rev":3}"#.to_string();
        assert_eq!(join_answer(&args(&["watch", "lifecycle"]), (0, plain.clone()), lookup), (0, plain.clone()));
        assert_eq!(join_answer(&args(&["watch", "lifecycle", "--rows"]), (0, plain.clone()), lookup), (0, plain));
        let failed = (2, r#"{"rows":[{"bead_id":"sp-a"}]}"#.to_string());
        assert_eq!(join_answer(&args(&["watch", "lifecycle", "--rows"]), failed.clone(), lookup), failed);
        let delivery = (0, r#"[{"bead_id":"sp-a"}]"#.to_string());
        assert_eq!(join_answer(&args(&["list", "--delivery"]), delivery.clone(), lookup), delivery);
        let garbled = (0, "not json".to_string());
        assert_eq!(join_answer(&args(&["watch", "lifecycle", "--rows"]), garbled.clone(), lookup), garbled);
    }

    #[test_id(ta1rokc)]
    #[test]
    fn a_listed_record_takes_its_title_and_priority_from_bd_in_the_dolt_listings_place() {
        let mut rows = vec![json!({"bead_id": "sp-a", "state": "READY", "repo": "harness"}), json!({"bead_id": "sp-gone", "state": "READY", "repo": "harness"})];
        let planned = BTreeMap::from([("sp-a".to_string(), (json!("the title"), json!(1)))]);
        merge_planning(&mut rows, &planned);
        let keys: Vec<&String> = rows[0].as_object().unwrap().keys().collect();
        let tail: Vec<&str> = keys[2..].iter().map(|k| k.as_str()).collect();
        assert_eq!(tail, ["title", "priority", "repo"], "{keys:?}");
        assert_eq!((rows[0]["title"].clone(), rows[0]["priority"].clone()), (json!("the title"), json!(1)));
        assert_eq!((rows[1]["title"].clone(), rows[1]["priority"].clone()), (Value::Null, Value::Null), "a record bd does not know stays untitled");
    }
}
