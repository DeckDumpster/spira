//! The planning plane's read contract. `plan items|edges|children --ids a,b,…` answer from bd's
//! own tables over this connection — never bd's CLI, never git — each with the planning
//! watermark read before the rows, so a poller that sees the watermark unchanged has seen every
//! write. `plan stamp` advances the watermark after a planning write and mirrors it into bd's
//! own kv. Read-only on bd: spira_lc holds SELECT on bd's issues, labels and dependencies.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use serde_json::{json, Value};

use crate::db::Conn;
use crate::rows;

/// Every bd column the reader names, by table: the conformance gate checks each exists in the
/// pinned bd, so a renamed column fails there and not as a claim check in production.
pub const READER_COLUMNS: &[(&str, &[&str])] = &[
    ("issues", &["id", "title", "priority", "issue_type", "status", "defer_until", "created_at", "updated_at", "closed_at", "description", "notes", "external_ref"]),
    ("labels", &["issue_id", "label"]),
    ("dependencies", &["issue_id", "depends_on_issue_id", "type"]),
];

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

/// The content store's edges plus the lifecycle's mirrored ones (`spira-lc dep-add` writes only
/// the latter), one per `(issue, target)`; the content store's row wins.
pub fn union_edges(deps: Vec<Value>, mirrored: Vec<Value>) -> Vec<Value> {
    let key = |d: &Value| (s(d, "issue_id").map(str::to_string), s(d, "depends_on_id").map(str::to_string));
    let seen: std::collections::BTreeSet<_> = deps.iter().map(key).collect();
    let mut out = deps;
    out.extend(mirrored.into_iter().filter(|d| !seen.contains(&key(d))));
    out
}

/// `plan items|edges|children|watermark|stamp|schema …`: JSON with the watermark on every read.
pub fn cmd_plan(args: &[String], conn: &Conn) -> (i32, String) {
    let rest = args.get(1..).unwrap_or_default();
    let answer = match args.first().map(String::as_str) {
        Some("items") => read_watermark(conn).and_then(|w| items(rest, conn).map(|v| json!({ "watermark": w, "items": v }))),
        Some("edges") => read_watermark(conn).and_then(|w| edges(rest, conn).map(|v| json!({ "watermark": w, "edges": v }))),
        Some("children") => read_watermark(conn).and_then(|w| children(rest, conn).map(|v| json!({ "watermark": w, "items": v }))),
        Some("watermark") => read_watermark(conn).map(|w| json!({ "watermark": w })),
        Some("stamp") => stamp(conn).map(|w| json!({ "watermark": w })),
        Some("schema") => return schema(conn),
        _ => Err(USAGE.into()),
    };
    match answer {
        Ok(v) => (0, v.to_string()),
        Err(e) => (CANNOT_TELL, e),
    }
}

const USAGE: &str = "usage: plan items (--ids a,b|- | --label L [--status a,b] | --external-contains S) [--full] [--open] | plan edges --ids a,b|- [--dependents] | plan children --ids a,b|- [--full] [--open] | plan watermark | plan stamp | plan schema";

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn ids_of(args: &[String]) -> Result<Vec<&str>, String> {
    let ids = flag(args, "--ids").ok_or("--ids <a,b,…> is required")?;
    Ok(ids.split(',').map(str::trim).filter(|x| !x.is_empty()).collect())
}

fn items(args: &[String], conn: &Conn) -> Result<Vec<Value>, String> {
    let full = args.iter().any(|a| a == "--full");
    let open = args.iter().any(|a| a == "--open");
    match (flag(args, "--ids"), flag(args, "--external-contains"), flag(args, "--label")) {
        (Some(_), None, None) => fetch_with(conn, &ids_of(args)?, full, open),
        (None, Some(needle), None) => by_external_ref(conn, needle, full, open),
        (None, None, Some(label)) => by_label(conn, label, flag(args, "--status"), full),
        _ => Err("plan items: exactly one of --ids <a,b,…>, --external-contains <text> or --label <label> [--status a,b] is required".into()),
    }
}

/// Raw edges with their types, out of `ids` (or, with `--dependents`, into them). Whether an
/// edge blocks is the readiness rule's to decide, not this reader's.
fn edges(args: &[String], conn: &Conn) -> Result<Vec<Value>, String> {
    let ids = ids_of(args)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    edges_where(conn, &ids, args.iter().any(|a| a == "--dependents"))
}

fn id_list(ids: &[&str]) -> String {
    ids.iter().map(|x| format!("'{}'", rows::escape(x))).collect::<Vec<_>>().join(",")
}

fn edges_where(conn: &Conn, ids: &[&str], into: bool) -> Result<Vec<Value>, String> {
    let cdb = content_db()?;
    let list = id_list(ids);
    let q = |sql: String| conn.query(&sql).map_err(|e| format!("cannot tell: {e:?}"));
    // bd's column is depends_on_issue_id (beside depends_on_wisp_id / depends_on_external);
    // only issue-to-issue edges are planning edges between beads.
    let (bd_side, lc_side) = if into { ("depends_on_issue_id", "depends_on") } else { ("issue_id", "bead_id") };
    let deps = q(format!(
        "SELECT issue_id, depends_on_issue_id AS depends_on_id, `type` FROM {cdb}.dependencies WHERE {bd_side} IN ({list}) AND depends_on_issue_id IS NOT NULL ORDER BY issue_id, depends_on_issue_id"
    ))?;
    let mirrored = q(format!("SELECT bead_id AS issue_id, depends_on AS depends_on_id, dep_type AS `type` FROM bead_dep WHERE {lc_side} IN ({list}) ORDER BY bead_id, depends_on"))?;
    Ok(union_edges(deps, mirrored)
        .into_iter()
        .map(|d| json!({ "issue_id": s(&d, "issue_id"), "depends_on_id": s(&d, "depends_on_id"), "type": s(&d, "type").unwrap_or("blocks") }))
        .collect())
}

/// The beads whose parent is one of `ids`, as item rows.
fn children(args: &[String], conn: &Conn) -> Result<Vec<Value>, String> {
    let ids = ids_of(args)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let kids: BTreeSet<String> = edges_where(conn, &ids, true)?
        .iter()
        .filter(|e| e["type"] == "parent-child")
        .filter_map(|e| s(e, "issue_id").map(str::to_string))
        .collect();
    let kids: Vec<&str> = kids.iter().map(String::as_str).collect();
    fetch_with(conn, &kids, args.iter().any(|a| a == "--full"), args.iter().any(|a| a == "--open"))
}

/// `plan schema`: every column [`READER_COLUMNS`] names that bd's database lacks. Exit 0 when
/// none is missing; otherwise 2, naming each as `table.column`.
fn schema(conn: &Conn) -> (i32, String) {
    let cdb = match content_db() {
        Ok(c) => c,
        Err(e) => return (CANNOT_TELL, e),
    };
    let sql = format!("SELECT table_name AS t, column_name AS c FROM information_schema.columns WHERE table_schema = '{}'", rows::escape(cdb.trim_matches('`')));
    let have: BTreeSet<(String, String)> = match conn.query(&sql) {
        Ok(rows) => rows.iter().filter_map(|r| Some((s(r, "t")?.to_lowercase(), s(r, "c")?.to_lowercase()))).collect(),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
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

pub fn with_planning_fields(answer: (i32, String), conn: &Conn) -> (i32, String) {
    join_planning(answer, &|ids| titles(conn, ids))
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

/// A bead's title and priority, as a listing shows them.
pub type Titled = (Value, Value);

/// Title and priority of `ids`, the two fields a listing joins: one query, no labels or edges.
fn titles(conn: &Conn, ids: &[&str]) -> Result<Vec<(String, Titled)>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let cdb = content_db()?;
    let rows = conn.query(&format!("SELECT id, title, priority FROM {cdb}.issues WHERE id IN ({})", id_list(ids))).map_err(|e| format!("cannot tell: {e:?}"))?;
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

/// The serving process's own connection, so a planning write made inside lc-serve stamps on it
/// instead of calling back into the socket it is serving.
static SERVED: OnceLock<Arc<Conn>> = OnceLock::new();

pub fn serve_on(conn: Arc<Conn>) {
    let _ = SERVED.set(conn);
}

/// The control plane's watermark: the state store's `planning/watermark` when one is
/// configured, else the lifecycle store's `planning_watermark` row. Never bd's kv, which is
/// the planning plane's copy for a restore.
pub fn read_watermark(conn: &Conn) -> Result<u64, String> {
    if let Some(store) = crate::control::served::configured() {
        let store = store.map_err(|e| format!("cannot tell: the control store: {e}"))?;
        return control_seq(&store, conn).map(|(seq, _)| seq);
    }
    lifecycle_seq(conn)
}

fn lifecycle_seq(conn: &Conn) -> Result<u64, String> {
    let rows = conn.query("SELECT seq FROM planning_watermark WHERE id = 1").map_err(|e| format!("cannot tell: the planning watermark: {e:?}"))?;
    Ok(rows.first().and_then(|r| r.get("seq")).and_then(|n| n.as_u64().or_else(|| n.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0))
}

type Control = Arc<crate::control::store::Store<state_store_sqlite::SqliteBackend>>;

/// The control store's sequence and the revision it was read at. Before the key's first write
/// it continues the lifecycle store's sequence, so moving onto the control store never moves
/// the watermark back.
fn control_seq(store: &Control, conn: &Conn) -> Result<(u64, Option<state_store::Rev>), String> {
    let key = crate::control::keys::PLANNING_WATERMARK;
    let item = store.contract().get(key).map_err(|e| format!("cannot tell: {key}: {e}"))?;
    match item {
        Some(i) => Ok((i.value.data.get("seq").and_then(Value::as_u64).unwrap_or(0), Some(i.mod_rev))),
        None => Ok((lifecycle_seq(conn).unwrap_or(0), None)),
    }
}

const STAMP_ATTEMPTS: usize = 16;

/// Advances the watermark by one, in one transaction, and returns the value it now holds.
pub fn stamp(conn: &Conn) -> Result<u64, String> {
    let seq = match crate::control::served::configured() {
        Some(store) => stamp_control(&store.map_err(|e| format!("cannot tell: the control store: {e}"))?, conn)?,
        None => stamp_lifecycle(conn)?,
    };
    mirror(seq);
    Ok(seq)
}

/// A commit that lost to a concurrent stamp is retried: a stamp lost is a write a poller misses.
fn stamp_lifecycle(conn: &Conn) -> Result<u64, String> {
    let mut last = String::new();
    for _ in 0..STAMP_ATTEMPTS {
        match conn.append_event("INSERT INTO planning_watermark (id, seq) VALUES (1, 1) ON DUPLICATE KEY UPDATE seq = seq + 1") {
            Ok(()) => return lifecycle_seq(conn),
            Err(e) => last = format!("{e:?}"),
        }
    }
    Err(format!("cannot tell: plan stamp: {last}"))
}

fn stamp_control(store: &Control, conn: &Conn) -> Result<u64, String> {
    use state_store::{Compare, Object, Request, Txn};
    let key = crate::control::keys::PLANNING_WATERMARK;
    for _ in 0..STAMP_ATTEMPTS {
        let (seq, rev) = control_seq(store, conn)?;
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

/// Stamps after a planning write this process made itself: on the serving connection inside
/// lc-serve, else through the service, else on a direct connection. A failed stamp is
/// reported and never fails the write, which has already happened.
pub fn stamp_after_write() {
    let done = if let Some(conn) = SERVED.get() {
        stamp(conn).map(|_| ())
    } else {
        match crate::client::try_socket(&["plan".to_string(), "stamp".to_string()]) {
            Some((0, _)) => Ok(()),
            Some((code, out)) => Err(format!("exit {code}: {out}")),
            None => Conn::from_env().map_err(|e| format!("{e:?}")).and_then(|c| stamp(&c).map(|_| ())),
        }
    };
    if let Err(e) = done {
        eprintln!("spira-lc: the planning write landed but the watermark did not advance: {e}");
    }
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
    let held = crate::bd::run(&["kv", "get", KV_KEY]).ok().and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0);
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
pub fn fetch(conn: &Conn, ids: &[&str]) -> Result<Vec<Value>, String> {
    fetch_with(conn, ids, false, false)
}

const TEXT_COLUMNS: &str = ", description, notes, external_ref";

fn by_external_ref(conn: &Conn, needle: &str, full: bool, open: bool) -> Result<Vec<Value>, String> {
    let cdb = content_db()?;
    if needle.is_empty() {
        return Err("plan items: --external-contains needs a non-empty text".into());
    }
    let sql = format!("SELECT id FROM {cdb}.issues WHERE LOCATE('{}', external_ref) > 0 ORDER BY id", rows::escape(needle));
    let found = conn.query(&sql).map_err(|e| format!("cannot tell: {e:?}"))?;
    let ids: Vec<&str> = found.iter().filter_map(|r| s(r, "id")).collect();
    fetch_with(conn, &ids, full, open)
}

/// The beads carrying `label`, narrowed to the comma-separated `statuses` when given.
fn by_label(conn: &Conn, label: &str, statuses: Option<&str>, full: bool) -> Result<Vec<Value>, String> {
    let cdb = content_db()?;
    if label.is_empty() {
        return Err("plan items: --label needs a non-empty label".into());
    }
    let status = match statuses {
        None => String::new(),
        Some(list) => format!(" AND status IN ({})", list.split(',').filter(|x| !x.is_empty()).map(|x| format!("'{}'", rows::escape(x))).collect::<Vec<_>>().join(",")),
    };
    let sql = format!("SELECT id FROM {cdb}.issues WHERE id IN (SELECT issue_id FROM {cdb}.labels WHERE label = '{}'){status} ORDER BY id", rows::escape(label));
    let found = conn.query(&sql).map_err(|e| format!("cannot tell: {e:?}"))?;
    let ids: Vec<&str> = found.iter().filter_map(|r| s(r, "id")).collect();
    fetch_with(conn, &ids, full, false)
}

fn fetch_with(conn: &Conn, ids: &[&str], full: bool, open: bool) -> Result<Vec<Value>, String> {
    // An empty id list is an empty answer: it needs no store, so it is decided before the
    // content database is resolved (r-auto-148: an unconfigured test process was refused here).
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let cdb = content_db()?;
    let list = ids.iter().map(|x| format!("'{}'", rows::escape(x))).collect::<Vec<_>>().join(",");
    let q = |sql: String| conn.query(&sql).map_err(|e| format!("cannot tell: {e:?}"));
    let text = if full { TEXT_COLUMNS } else { "" };
    let live = if open { " AND status <> 'closed'" } else { "" };
    let issues = q(format!(
        "SELECT id, title, priority, issue_type, status, DATE_FORMAT(defer_until, '{RFC3339}') AS defer_until, DATE_FORMAT(created_at, '{RFC3339}') AS created_at, DATE_FORMAT(updated_at, '{RFC3339}') AS updated_at{text} \
         FROM {cdb}.issues WHERE id IN ({list}){live} ORDER BY id"
    ))?;
    let labels = q(format!("SELECT issue_id, label FROM {cdb}.labels WHERE issue_id IN ({list}) ORDER BY issue_id, label"))?;
    let deps = edges_where(conn, ids, false)?;
    let mut out = assemble(&issues, &labels, &deps);
    if full {
        for (row, issue) in out.iter_mut().zip(issues.iter().filter(|i| s(i, "id").is_some())) {
            for col in ["description", "notes", "external_ref"] {
                row[col] = json!(issue.get(col).and_then(Value::as_str).unwrap_or(""));
            }
        }
    }
    Ok(out)
}

/// The pane's window on the content store: live beads plus the last day's closures.
const PANE_SCOPE: &str = "(status <> 'closed' OR closed_at >= UTC_TIMESTAMP() - INTERVAL 1 DAY)";
const INFLOW_ROWS: usize = 40;

fn count(conn: &Conn, sql: String) -> Result<i64, String> {
    let rows = conn.query(&sql).map_err(|e| format!("cannot tell: {e:?}"))?;
    Ok(rows.first().and_then(|r| r.get("n")).and_then(|n| n.as_i64().or_else(|| n.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0))
}

/// `pane-facets inflow --window-secs N`: what the ops pane's inflow section draws, aggregated
/// here so the reply is the newest rows plus counts, never the table.
/// `pane-facets repo-labels --valid a,b`: live beads with no `repo:` label, and with only
/// `repo:` labels outside the valid set.
pub fn cmd_pane_facets(args: &[String], conn: &Conn) -> (i32, String) {
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1));
    let out = match args.first().map(String::as_str) {
        Some("inflow") => flag("--window-secs").and_then(|w| w.parse::<u32>().ok()).ok_or_else(|| "pane-facets inflow: --window-secs <n> is required".to_string()).and_then(|w| inflow(conn, w)),
        Some("repo-labels") => repo_labels(conn, flag("--valid").map_or("", String::as_str)),
        _ => Err("usage: pane-facets inflow --window-secs N | repo-labels --valid a,b".to_string()),
    };
    match out {
        Ok(v) => (0, v.to_string()),
        Err(e) => (CANNOT_TELL, e),
    }
}

fn inflow(conn: &Conn, window_secs: u32) -> Result<Value, String> {
    let cdb = content_db()?;
    let q = |sql: String| conn.query(&sql).map_err(|e| format!("cannot tell: {e:?}"));
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

fn repo_labels(conn: &Conn, valid: &str) -> Result<Value, String> {
    let cdb = content_db()?;
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

    /// sp-jf7spl: the content database is the one the store declares, never a default.
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

    #[test]
    fn a_bead_with_no_labels_or_edges_has_empty_lists() {
        let out = assemble(&v(r#"[{"id":"sp-a","priority":"2"}]"#), &[], &[]);
        assert_eq!((out[0]["labels"].clone(), out[0]["dependencies"].clone(), out[0]["parent"].clone()), (json!([]), json!([]), Value::Null));
    }

    fn a(x: &[&str]) -> Vec<String> {
        x.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_ids_is_an_empty_answer_and_a_missing_selector_is_refused() {
        let c = Conn::unreachable();
        assert_eq!(items(&a(&["--ids", ""]), &c), Ok(vec![]));
        assert_eq!(children(&a(&["--ids", ","]), &c), Ok(vec![]));
        assert_eq!(edges(&a(&["--ids", ""]), &c), Ok(vec![]));
        assert!(items(&[], &c).is_err());
        assert!(edges(&[], &c).unwrap_err().contains("--ids"));
        assert_eq!(cmd_plan(&a(&["nope"]), &c).0, CANNOT_TELL);
    }

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

#[cfg(test)]
mod union_tests {
    use super::*;

    #[test]
    fn a_mirrored_edge_blocks_and_a_duplicate_is_not_doubled() {
        let deps = vec![json!({"issue_id": "sp-b", "depends_on_id": "sp-a", "type": "blocks"})];
        let mirrored = vec![
            json!({"issue_id": "sp-b", "depends_on_id": "sp-a", "type": "blocks"}),
            json!({"issue_id": "sp-b", "depends_on_id": "sp-c", "type": "blocks"}),
        ];
        let rows = assemble(&[json!({"id": "sp-b"})], &[], &union_edges(deps, mirrored));
        assert_eq!(rows[0]["dependencies"].as_array().unwrap().len(), 2);
    }
}
