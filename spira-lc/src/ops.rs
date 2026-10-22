//! The ops read model (`lifecycle/migrations/0007-ops-read-model.sql`, `0009-where-stuck.sql`):
//! `ops-view <view>` returns one view as a JSON array, `ops-graph` the machine's legal edges, and `backfill-titles` fills the mirrored
//! title and priority of rows filed before the mirror existed. Both ride `dispatch`, so a
//! refresh through `spira-lc serve` is one query on the held connection.

use serde_json::Value;

use crate::db::Conn;

/// The only views `ops-view` will name; the argument is matched, never interpolated.
pub const VIEWS: [&str; 6] = ["ops_live", "ops_round", "ops_recent", "ops_edges", "ops_dwell", "ops_dwell_p95"];

/// Every applied state change of the last day, read from the event time index.
pub const GANTT_SQL: &str = "SELECT seq, lc_key AS bead_id, from_state, to_state, at FROM event \
     WHERE machine = 'bead' AND applied = 1 AND at >= UNIX_TIMESTAMP() - 86400 AND from_state <> to_state";

const LIVE_STATES: &str = "'OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK'";

/// Terminal beads of the last day, one index range per state.
fn terminal_sql() -> String {
    let states = lifecycle::bead::BeadState::TERMINAL.iter().map(|s| format!("'{}'", s.as_str())).collect::<Vec<_>>().join(", ");
    format!("SELECT bead_id, state, since, updated_at, priority, title FROM bead WHERE state IN ({states}) AND since >= UNIX_TIMESTAMP() - 86400")
}

const EJECTS_SQL: &str = "SELECT lc_key AS batch_id, evidence, actor, at FROM event \
     WHERE machine = 'batch' AND event = 'Eject' AND applied = 1 AND at >= UNIX_TIMESTAMP() - 86400";

/// Everything the ops pane draws, in one process start on one held connection.
pub fn cmd_ops_snapshot(_args: &[String], conn: &Conn) -> (i32, String) {
    let queries: [(&str, String); 7] = [
        ("live", "SELECT * FROM ops_live".into()),
        ("recent", terminal_sql()),
        ("edges", "SELECT * FROM ops_edges".into()),
        ("dwell_p95", "SELECT * FROM ops_dwell_p95".into()),
        ("dwell", "SELECT * FROM ops_dwell".into()),
        ("batches", "SELECT * FROM ops_round".into()),
        ("ejects", EJECTS_SQL.into()),
    ];
    let mut out = serde_json::Map::new();
    out.insert("now".into(), serde_json::json!(crate::db::now_epoch()));
    out.insert("graph".into(), serde_json::from_str(&graph_json()).unwrap_or(Value::Null));
    for (key, sql) in &queries {
        match conn.query(sql) {
            Ok(rows) => out.insert((*key).into(), Value::Array(rows)),
            Err(e) => return (2, format!("cannot tell: {key}: {e:?}")),
        };
    }
    (0, Value::Object(out).to_string())
}

/// `ops-view <view> [--epics]`: one view as a JSON array; with `--epics`, an object
/// `{"rows": [...], "epics": [...]}` so the pane gets the epic rollup on a call it already makes.
pub fn cmd_ops_view(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(view) = args.first().and_then(|a| VIEWS.iter().find(|v| **v == a.as_str())) else {
        return (2, format!("ops-view: takes one of {}", VIEWS.join(" | ")));
    };
    let rows = match conn.live().and_then(|l| l.ops_view(view, crate::db::now_epoch())) {
        Some(rows) => rows,
        None => match view_cached(conn, view, crate::db::now_epoch()) {
            Ok(rows) => rows,
            Err(e) => return (2, format!("cannot tell: {e}")),
        },
    };
    if !args.iter().any(|a| a == "--epics") {
        return (0, Value::Array(rows).to_string());
    }
    match epics_cached(conn, crate::db::now_epoch()) {
        Ok(epics) => (0, serde_json::json!({ "rows": rows, "epics": epics }).to_string()),
        Err(e) => (2, format!("cannot tell: epics: {e}")),
    }
}

// ───────────────────────────── views computed in Dolt, kept warm by the sweeper

/// Views whose SQL scans the event history; a serving process answers them from memory.
const SLOW_VIEWS: [&str; 4] = ["ops_edges", "ops_dwell", "ops_dwell_p95", "ops_round"];

const VIEW_TTL_SECS: i64 = 30;

/// Views the serving process answers from memory; it keeps the last answer so a reload of
/// memory never sends the request to Dolt.
const MEMORY_VIEWS: [&str; 2] = ["ops_live", "ops_recent"];

const MEMORY_VIEW_MAX_AGE_SECS: i64 = 60;

type ViewCache = std::collections::BTreeMap<&'static str, (i64, Vec<Value>)>;

static VIEW_CACHE: std::sync::Mutex<ViewCache> = std::sync::Mutex::new(std::collections::BTreeMap::new());

fn view_fetch(conn: &Conn, view: &'static str, now: i64) -> Result<Vec<Value>, String> {
    let rows = conn.query(&format!("SELECT * FROM {view}")).map_err(|e| format!("{e:?}"))?;
    if let Ok(mut c) = VIEW_CACHE.lock() {
        c.insert(view, (now, rows.clone()));
    }
    Ok(rows)
}

fn remember_memory_views(conn: &Conn, now: i64) {
    for name in MEMORY_VIEWS {
        if let (Some(rows), Ok(mut c)) = (conn.live().and_then(|l| l.ops_view(name, now)), VIEW_CACHE.lock()) {
            c.insert(name, (now, rows));
        }
    }
}

fn view_cached(conn: &Conn, view: &str, now: i64) -> Result<Vec<Value>, String> {
    if conn.live.is_some() {
        if let Some(name) = MEMORY_VIEWS.iter().find(|v| **v == view) {
            if let Some((at, rows)) = VIEW_CACHE.lock().ok().and_then(|c| c.get(name).cloned()) {
                if now - at < MEMORY_VIEW_MAX_AGE_SECS {
                    return Ok(rows);
                }
            }
        }
    }
    let Some(name) = SLOW_VIEWS.iter().find(|v| **v == view) else {
        return conn.query(&format!("SELECT * FROM {view}")).map_err(|e| format!("{e:?}"));
    };
    if conn.live.is_some() {
        if let Some((at, rows)) = VIEW_CACHE.lock().ok().and_then(|c| c.get(name).cloned()) {
            if now - at < EPICS_MAX_AGE_SECS {
                return Ok(rows);
            }
        }
    }
    view_fetch(conn, name, now)
}

/// The serving process recomputes the slow views off the request path.
pub fn views_tick(conn: &Conn) {
    if conn.live.is_none() {
        return;
    }
    let now = crate::db::now_epoch();
    remember_memory_views(conn, now);
    for name in SLOW_VIEWS {
        let stale = VIEW_CACHE.lock().ok().and_then(|c| c.get(name).map(|(at, _)| now - at >= VIEW_TTL_SECS)).unwrap_or(true);
        if stale {
            if let Err(e) = view_fetch(conn, name, now) {
                eprintln!("spira-lc serve: {name} not refreshed: {e}");
            }
        }
    }
}

// ───────────────────────────── epics: open title series, rolled up (per Ryan 2026-10-13)

/// An epic is a title series, `<name> <n>:` or `<name> <n>/<m>:`, the shape design
/// decompositions file under. One aggregate over the bead table: a row per (repo, series,
/// state, the hold reason of a manually held live bead), a few hundred at most.
pub const EPICS_SQL: &str = "SELECT repo, REGEXP_REPLACE(title, '^(.+?) [0-9]+(/[0-9]+)?: .*$', '$1') AS series, state, \
     CASE WHEN state NOT IN ('LANDED', 'DONE', 'DROPPED', 'SUPERSEDED') AND holds LIKE '%\"manual\"%' THEN reason END AS held_reason, \
     COUNT(*) AS n, MIN(updated_at) AS since FROM bead WHERE title REGEXP '^.+ [0-9]+(/[0-9]+)?: ' \
     GROUP BY repo, series, state, held_reason";

/// The rollup is recomputed at most this often in a long-lived server; epics move in minutes.
const EPICS_TTL_SECS: i64 = 60;

static EPICS_CACHE: std::sync::Mutex<Option<(i64, Value)>> = std::sync::Mutex::new(None);

/// Past this age even a serving process recomputes on the request, in case its sweeper is down.
const EPICS_MAX_AGE_SECS: i64 = 600;

fn epics_cached(conn: &Conn, now: i64) -> Result<Value, String> {
    let ttl = if conn.live.is_some() { EPICS_MAX_AGE_SECS } else { EPICS_TTL_SECS };
    if let Some((at, v)) = EPICS_CACHE.lock().ok().and_then(|c| c.clone()) {
        if now - at < ttl {
            return Ok(v);
        }
    }
    epics_refresh(conn, now)
}

fn epics_refresh(conn: &Conn, now: i64) -> Result<Value, String> {
    let rows = conn.query(EPICS_SQL).map_err(|e| format!("{e:?}"))?;
    let v = epics_rollup(&rows);
    if let Ok(mut c) = EPICS_CACHE.lock() {
        *c = Some((now, v.clone()));
    }
    Ok(v)
}

/// The serving process keeps the rollup warm from its sweeper, so no request pays for the aggregate.
pub fn epics_tick(conn: &Conn) {
    let now = crate::db::now_epoch();
    let stale = EPICS_CACHE.lock().ok().and_then(|c| c.as_ref().map(|(at, _)| now - at >= EPICS_TTL_SECS)).unwrap_or(true);
    if conn.live.is_some() && stale {
        if let Err(e) = epics_refresh(conn, now) {
            eprintln!("spira-lc serve: epics not refreshed: {e}");
        }
    }
}

fn n_of(v: &Value) -> i64 {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0)
}

/// Which count a state adds to; `None` for a terminal state that is not a delivery.
fn epic_bucket(state: &str) -> Option<&'static str> {
    match state {
        "LANDED" | "DONE" => Some("landed"),
        "DROPPED" | "SUPERSEDED" => None,
        "CERTIFIED" => Some("certified"),
        "REWORK" => Some("rework"),
        "WORKING" | "SUBMITTED" | "IN_DELIVERY" => Some("building"),
        _ => Some("waiting"),
    }
}

/// The first clause of a manual hold's reason, without its review-by stamp.
pub fn hold_label(reason: &str) -> String {
    let r = reason.trim();
    let r = match r.strip_prefix("review-by:") {
        Some(rest) => rest.split_once(char::is_whitespace).map(|(_, t)| t).unwrap_or("").trim_start(),
        None => r,
    };
    let r = r.split(" (").next().unwrap_or(r);
    let r = r.split([':', ';', ',']).next().unwrap_or(r).trim();
    if r.is_empty() { "no reason given".into() } else { r.to_string() }
}

/// Grouped rows in, one object per epic that still has a member to deliver out, ordered by
/// repo then series: counts, the oldest manual hold's time, and the holds by label.
pub fn epics_rollup(rows: &[Value]) -> Value {
    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Acc {
        counts: BTreeMap<&'static str, i64>,
        holds: BTreeMap<String, i64>,
        held_since: Option<i64>,
    }
    let mut acc: BTreeMap<(String, String), Acc> = BTreeMap::new();
    for r in rows {
        let (Some(series), Some(state)) = (r["series"].as_str(), r["state"].as_str()) else { continue };
        let Some(b) = epic_bucket(state) else { continue };
        let repo = r["repo"].as_str().unwrap_or("").to_string();
        let a = acc.entry((repo, series.to_string())).or_default();
        let n = n_of(&r["n"]);
        *a.counts.entry(b).or_default() += n;
        if let Some(reason) = r["held_reason"].as_str() {
            *a.holds.entry(hold_label(reason)).or_default() += n;
            let since = n_of(&r["since"]);
            a.held_since = Some(a.held_since.map_or(since, |s| s.min(since)));
        }
    }
    let out = acc
        .into_iter()
        .filter(|(_, a)| a.counts.iter().any(|(k, n)| *k != "landed" && *n > 0))
        .map(|((repo, series), a)| {
            let mut holds: Vec<(String, i64)> = a.holds.into_iter().collect();
            holds.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
            let c = |k: &str| a.counts.get(k).copied().unwrap_or(0);
            serde_json::json!({
                "repo": repo, "epic": series,
                "landed": c("landed"), "certified": c("certified"), "rework": c("rework"), "building": c("building"), "waiting": c("waiting"),
                "held_since": a.held_since,
                "holds": holds.into_iter().map(|(label, n)| serde_json::json!({"label": label, "n": n})).collect::<Vec<_>>(),
            })
        })
        .collect();
    Value::Array(out)
}

/// `live-check`: compare the live rows with Dolt now. A mismatch reloads memory and raises an
/// incident naming the divergent beads (exit 1); a match is exit 0.
pub fn cmd_live_check(conn: &Conn) -> (i32, String) {
    match run_live_check(conn, &raise_incident) {
        Ok(check) => (i32::from(!check.divergent.is_empty()), serde_json::json!({ "consistent": check.divergent.is_empty(), "rows": check.rows, "memory_hash": check.memory_hash, "dolt_hash": check.dolt_hash, "divergent": check.divergent }).to_string()),
        Err(e) => (2, format!("live-check: {e}")),
    }
}

pub fn run_live_check(conn: &Conn, raise: &dyn Fn(&[String], &str)) -> Result<crate::live::Check, String> {
    let check = conn.live_check()?;
    if !check.divergent.is_empty() && !check.explained {
        raise(&check.divergent, &last_writers(conn, &check.divergent));
    }
    Ok(check)
}

/// The newest event of each bead, as `bead: actor event applied@at`. A row that changed with no
/// event newer than the reload point names no lc-serve writer: it was written out of band.
pub fn last_writers(conn: &Conn, ids: &[String]) -> String {
    let list = ids.iter().map(|i| format!("'{}'", i.replace('\\', "\\\\").replace('\'', "\\'"))).collect::<Vec<_>>().join(",");
    let sql = format!("SELECT e.lc_key, e.actor, e.event, e.applied, e.at FROM event e WHERE e.machine = 'bead' AND e.lc_key IN ({list}) AND e.seq = (SELECT MAX(seq) FROM event WHERE machine = 'bead' AND lc_key = e.lc_key)");
    match conn.query(&sql) {
        Ok(rows) => rows.iter().map(|r| format!("{}: {} {} applied={}@{}", field(r, "lc_key"), field(r, "actor"), field(r, "event"), field(r, "applied"), field(r, "at"))).collect::<Vec<_>>().join("\n"),
        Err(e) => format!("last events unreadable: {e:?}"),
    }
}

fn field(r: &Value, k: &str) -> String {
    r.get(k).map_or(String::new(), |v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
}

pub fn raise_incident(divergent: &[String], writers: &str) {
    let title = format!("lc-serve live rows diverged from Dolt: {}", divergent.iter().take(5).cloned().collect::<Vec<_>>().join(", "));
    let body = format!("The consistency check found these beads in memory differing from the lifecycle store, and reloaded them from Dolt:\n{}\n\nNewest event of each (a row whose newest event predates the divergence was written out of band):\n{writers}\n\nA row changed without passing through lc-serve's write path, or a write's refresh was lost.\n", divergent.join("\n"));
    let env = [("SPIRA_INCIDENT_TYPE", "bug"), ("SPIRA_INCIDENT_PRIORITY", "1"), ("SPIRA_INCIDENT_ACTOR", "lc-serve"), ("SPIRA_INCIDENT_REPO", "spira"), ("SPIRA_INCIDENT_REF", "lc-live-divergence"), ("SPIRA_INCIDENT_CAUSE", "lc-live-divergence")];
    let (code, out) = crate::bd::tool_env("incident.sh", &["file".to_string(), title, "-".to_string()], Some(&body), "lc-serve", 60, &env);
    if code != 0 {
        eprintln!("spira-lc: live-check could not raise its incident ({code}): {out}");
    }
}

/// The stuck page's bars: the day's transitions plus the clock they are measured against.
pub fn cmd_ops_gantt(args: &[String], conn: &Conn) -> (i32, String) {
    if args.first().map(String::as_str) == Some("--print-sql") {
        return (0, GANTT_SQL.to_string());
    }
    match conn.query(GANTT_SQL) {
        Ok(rows) => (0, serde_json::json!({ "now": crate::db::now_epoch(), "events": rows }).to_string()),
        Err(e) => (2, format!("cannot tell: {e:?}")),
    }
}

/// Refused bead events inside the last `window_secs`, one row per (actor, event, from_state,
/// refusal) with the count, the span and the newest five `bead@epoch` examples.
pub fn refusals_sql(window_secs: u64) -> String {
    format!(
        "SELECT actor, event, from_state, refusal, COUNT(*) AS n, MIN(at) AS first_at, MAX(at) AS last_at, \
         SUBSTRING_INDEX(GROUP_CONCAT(CONCAT(lc_key, '@', at) ORDER BY at DESC SEPARATOR ','), ',', 5) AS examples \
         FROM event WHERE machine = 'bead' AND applied = 0 AND at >= UNIX_TIMESTAMP() - {window_secs} \
         GROUP BY actor, event, from_state, refusal"
    )
}

/// `ops-refusals <window-secs>`: the refused bead events of the window, grouped by who asked.
pub fn cmd_ops_refusals(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(window) = args.first().and_then(|a| a.parse::<u64>().ok()).filter(|w| *w > 0) else {
        return (2, "ops-refusals: takes one positive <window-secs>".into());
    };
    match conn.query(&refusals_sql(window)) {
        Ok(rows) => (0, serde_json::json!({ "now": crate::db::now_epoch(), "classes": rows }).to_string()),
        Err(e) => (2, format!("cannot tell: {e:?}")),
    }
}

/// One bead's whole timeline, refused events included, with the row that says who holds it.
pub fn cmd_ops_bead(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(id) = args.first() else {
        return (2, "ops-bead: missing <bead-id>".into());
    };
    let key = crate::rows::escape(id);
    let bead = conn.query(&format!("SELECT bead_id, state, holder, persona, lease_until, holds, reason, gate_key, title, priority, updated_at FROM bead WHERE bead_id = '{key}'"));
    let events = conn.query(&format!("SELECT seq, event, from_state, to_state, applied, refusal, evidence, actor, at FROM event WHERE machine = 'bead' AND lc_key = '{key}' ORDER BY seq"));
    match (bead, events) {
        (Ok(b), Ok(e)) if b.is_empty() && e.is_empty() => (1, "{}".into()),
        (Ok(b), Ok(e)) => (0, serde_json::json!({ "now": crate::db::now_epoch(), "bead": b.first(), "events": e }).to_string()),
        (Err(e), _) | (_, Err(e)) => (2, format!("cannot tell: {e:?}")),
    }
}

/// The bead machine's legal moves, read from its own transition table.
pub fn cmd_ops_graph(_args: &[String], _conn: &Conn) -> (i32, String) {
    (0, graph_json())
}

/// States spelled as the `bead.state` column holds them, so a pane joins edges to view rows.
fn graph_json() -> String {
    let edges: Vec<Value> = lifecycle::bead::legal_edges().iter().map(|e| serde_json::json!({"from": e.from.as_str(), "event": e.event, "to": e.to.as_str()})).collect();
    Value::Array(edges).to_string()
}

pub fn title_priority(bd_show_json: &str) -> Option<(String, i64)> {
    let parsed: Value = serde_json::from_str(bd_show_json.trim()).ok()?;
    let doc = match &parsed {
        Value::Array(a) => a.first()?.clone(),
        v => v.clone(),
    };
    Some((doc.get("title")?.as_str()?.to_string(), doc.get("priority")?.as_i64()?))
}

/// Rows the views show that have no title yet: every live row, and the landings of the last day.
fn untitled_sql() -> String {
    format!(
        "SELECT bead_id FROM bead WHERE title IS NULL AND (state IN ({LIVE_STATES}) OR (state = 'LANDED' AND since >= UNIX_TIMESTAMP() - 86400))"
    )
}

pub fn cmd_backfill_titles(_args: &[String], conn: &Conn) -> (i32, String) {
    let ids: Vec<String> = match conn.query(&untitled_sql()) {
        Ok(rows) => rows.iter().filter_map(|r| r.get("bead_id").and_then(|v| v.as_str()).map(str::to_string)).collect(),
        Err(e) => return (2, format!("cannot tell: {e:?}")),
    };
    let (mut filled, mut missed) = (0, Vec::new());
    for id in &ids {
        let known = crate::bd::run(&["show", id, "--json"]).ok().and_then(|o| title_priority(&o));
        let Some((title, priority)) = known else {
            missed.push(id.clone());
            continue;
        };
        let args = [id.clone(), "--title".into(), title, "--priority".into(), priority.to_string()];
        match crate::cutover::cmd_create_bead(&args, conn) {
            (0, _) => filled += 1,
            (_, why) => missed.push(format!("{id} ({why})")),
        }
    }
    let code = if missed.is_empty() { 0 } else { 1 };
    (code, format!("backfill-titles: {filled} of {} filled{}", ids.len(), if missed.is_empty() { String::new() } else { format!("; not filled: {}", missed.join(", ")) }))
}

/// The `repo:<name>` label of a `bd show --json` document, array or object.
pub fn repo_label(bd_show_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(bd_show_json.trim()).ok()?;
    let doc = match &parsed {
        Value::Array(a) => a.first()?.clone(),
        v => v.clone(),
    };
    doc.get("labels")?.as_array()?.iter().filter_map(Value::as_str).find_map(|l| l.strip_prefix("repo:")).map(str::to_string)
}

/// `backfill-repos [id...]`: fills `bead.repo` from the content store's label on the named rows, or on every live row without one.
pub fn cmd_backfill_repos(args: &[String], conn: &Conn) -> (i32, String) {
    let ids: Vec<String> = if args.is_empty() {
        let sql = format!("SELECT bead_id FROM bead WHERE repo IS NULL AND state IN ({LIVE_STATES})");
        match conn.query(&sql) {
            Ok(rows) => rows.iter().filter_map(|r| r.get("bead_id").and_then(|v| v.as_str()).map(str::to_string)).collect(),
            Err(e) => return (2, format!("cannot tell: {e:?}")),
        }
    } else {
        args.to_vec()
    };
    let (mut filled, mut missed) = (0, Vec::new());
    for id in &ids {
        let Some(repo) = crate::bd::run(&["show", id, "--json"]).ok().and_then(|o| repo_label(&o)) else {
            missed.push(id.clone());
            continue;
        };
        let done = if args.is_empty() { crate::cutover::cmd_create_bead(&[id.clone(), "--repo".into(), repo], conn) } else { cmd_set_repo(&[id.clone(), repo], conn) };
        match done {
            (0, _) => filled += 1,
            (_, why) => missed.push(format!("{id} ({why})")),
        }
    }
    let code = if missed.is_empty() { 0 } else { 1 };
    (code, format!("backfill-repos: {filled} of {} filled{}", ids.len(), if missed.is_empty() { String::new() } else { format!("; not filled: {}", missed.join(", ")) }))
}

/// The refusal for a repo change on a bead a session is working.
pub fn working_refusal(id: &str) -> String {
    format!("set-repo: {id} is WORKING in a worktree of its current repo; release the claim first (spira-lc unclaim {id} <actor>), then change the repo label")
}

/// `set-repo <bead-id> <repo>|--clear [--only-if <repo>]`: points the lifecycle row at a new repo, which claims,
/// worktrees and rounds read. Refused for a WORKING row; a bead with no row is left alone.
pub fn cmd_set_repo(args: &[String], conn: &Conn) -> (i32, String) {
    use crate::cutover::{is_row_key, q};
    let (Some(id), Some(target)) = (args.first(), args.get(1)) else {
        return (2, "set-repo: usage: set-repo <bead-id> <repo>|--clear [--only-if <repo>]".into());
    };
    if !is_row_key(id) {
        return (2, format!("set-repo: {id:?} is not a bead id"));
    }
    let only_if = args.iter().position(|a| a == "--only-if").and_then(|i| args.get(i + 1));
    let value = if target == "--clear" { "NULL".to_string() } else { q(target) };
    let rows = match conn.query(&format!("SELECT state, repo FROM bead WHERE bead_id = {}", q(id))) {
        Ok(r) => r,
        Err(e) => return (2, format!("cannot tell: {e:?}")),
    };
    let Some(row) = rows.first() else { return (0, format!("set-repo: {id} has no lifecycle row")) };
    if let Some(want) = only_if {
        if row.get("repo").and_then(|v| v.as_str()) != Some(want.as_str()) {
            return (0, format!("set-repo: {id} does not name {want}; unchanged"));
        }
    }
    if row.get("state").and_then(|v| v.as_str()) == Some("WORKING") {
        return (1, working_refusal(id));
    }
    let sql = format!("UPDATE bead SET repo = {value} WHERE bead_id = {} AND state <> 'WORKING';\n", q(id));
    match conn.run_plain_touching(&sql, crate::live::Touch::Keys(vec![id.clone()])) {
        Ok(()) => (0, format!("set-repo: {id} -> {}", if target == "--clear" { "(none)" } else { target })),
        Err(e) => (2, format!("cannot tell: {e:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_serving_process_answers_the_slow_views_from_memory() {
        let mut conn = Conn::unreachable();
        let now = crate::db::now_epoch();
        assert!(view_cached(&conn, "ops_dwell", now).is_err(), "the unreachable store is the positive control");
        VIEW_CACHE.lock().unwrap().insert("ops_dwell", (now, vec![serde_json::json!({"bead_id": "sp-a"})]));
        assert!(view_cached(&conn, "ops_dwell", now).is_err(), "a one-shot process never trusts memory");
        conn.enable_live();
        let rows = view_cached(&conn, "ops_dwell", now).expect("served from memory");
        assert_eq!(rows.len(), 1);
        assert!(view_cached(&conn, "ops_dwell", now + EPICS_MAX_AGE_SECS + 1).is_err(), "past the max age it asks the store");
    }

    #[test]
    fn a_working_refusal_names_the_release_step() {
        let m = working_refusal("sp-x");
        assert!(m.contains("release the claim") && m.contains("unclaim sp-x"), "{m}");
    }

    fn grouped(repo: &str, series: &str, state: &str, held: Option<&str>, n: i64, since: i64) -> Value {
        serde_json::json!({"repo": repo, "series": series, "state": state, "held_reason": held, "n": n.to_string(), "since": since})
    }

    #[test]
    fn an_epic_counts_its_members_by_state_and_names_its_holds() {
        let rows = vec![
            grouped("deckdumpster", "deckdumpster multi-tenancy", "LANDED", None, 2, 10),
            grouped("deckdumpster", "deckdumpster multi-tenancy", "CERTIFIED", Some("review-by:2026-10-13 stacked on sp-t5980r, held"), 3, 500),
            grouped("deckdumpster", "deckdumpster multi-tenancy", "CERTIFIED", None, 1, 20),
            grouped("deckdumpster", "deckdumpster multi-tenancy", "REWORK", Some("review-by:2026-10-13 stacked on sp-t5980r, held"), 5, 400),
            grouped("deckdumpster", "deckdumpster multi-tenancy", "READY", Some("review-by:2026-10-19 prod cutover runbook (destructive)"), 1, 300),
            grouped("deckdumpster", "deckdumpster multi-tenancy", "DROPPED", None, 4, 1),
        ];
        let e = epics_rollup(&rows);
        let mt = &e[0];
        assert_eq!((mt["landed"].as_i64(), mt["certified"].as_i64(), mt["rework"].as_i64(), mt["waiting"].as_i64()), (Some(2), Some(4), Some(5), Some(1)), "{e}");
        assert_eq!(mt["held_since"], 300, "the oldest manual hold: {e}");
        assert_eq!(mt["holds"][0], serde_json::json!({"label": "stacked on sp-t5980r", "n": 8}), "{e}");
        assert_eq!(mt["holds"][1]["label"], "prod cutover runbook", "{e}");
    }

    #[test]
    fn an_epic_with_every_member_delivered_is_not_shown() {
        let rows = vec![grouped("spira", "round EC2 spill", "LANDED", None, 4, 1), grouped("spira", "round EC2 spill", "SUPERSEDED", None, 1, 1)];
        assert_eq!(epics_rollup(&rows), serde_json::json!([]));
    }

    #[test]
    fn a_hold_label_is_the_first_clause_without_its_stamp() {
        assert_eq!(hold_label("review-by:2026-10-19 Ryan's ruling: deckdumpster lakehouse work"), "Ryan's ruling");
        assert_eq!(hold_label("not dispatchable to aeons: the EC2 spill proof"), "not dispatchable to aeons");
        assert_eq!(hold_label("review-by:2026-10-19 "), "no reason given");
    }

    #[test]
    fn the_epic_rollup_is_one_grouped_query_over_title_series() {
        assert!(EPICS_SQL.contains("GROUP BY repo, series, state, held_reason") && EPICS_SQL.matches("SELECT").count() == 1, "{EPICS_SQL}");
    }

    #[test]
    fn the_repo_label_is_read_from_bd_show_json() {
        assert_eq!(repo_label(r#"[{"labels":["plan","repo:spira"]}]"#), Some("spira".into()));
        assert_eq!(repo_label(r#"{"labels":["plan"]}"#), None);
        assert_eq!(repo_label("not json"), None);
    }

    #[test]
    fn bd_show_json_gives_title_and_priority_whether_array_or_object() {
        assert_eq!(title_priority(r#"[{"title":"t","priority":1}]"#), Some(("t".into(), 1)));
        assert_eq!(title_priority(r#"{"title":"t","priority":0}"#), Some(("t".into(), 0)));
        assert_eq!(title_priority(r#"{"title":"t"}"#), None);
        assert_eq!(title_priority("not json"), None);
    }

    #[test]
    fn the_refusal_query_reads_only_refused_bead_events_of_the_window() {
        let q = refusals_sql(86400);
        assert!(q.contains("machine = 'bead' AND applied = 0") && q.contains("UNIX_TIMESTAMP() - 86400"), "{q}");
        assert!(q.contains("GROUP BY actor, event, from_state, refusal"), "{q}");
    }

    #[test]
    fn the_graph_is_json_edges_with_state_names_and_events() {
        let out = graph_json();
        let edges: Vec<Value> = serde_json::from_str(&out).unwrap();
        assert!(edges.iter().any(|e| e["from"] == "READY" && e["event"] == "Claim" && e["to"] == "WORKING"), "{out}");
    }

    #[test]
    fn the_backfill_reaches_exactly_the_rows_the_views_show() {
        let sql = untitled_sql();
        let migration = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../lifecycle/migrations/0007-ops-read-model.sql")).unwrap();
        let one_line = migration.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(one_line.contains(&format!("state IN ({LIVE_STATES})")), "ops_live and the backfill name the same states");
        assert!(sql.contains("state = 'LANDED' AND since >= UNIX_TIMESTAMP() - 86400") && migration.contains("state = 'LANDED' AND since >= UNIX_TIMESTAMP() - 86400"));
    }

    fn production_sized_bead(i: usize) -> Value {
        let live = i % 50 == 0;
        let mut o = serde_json::Map::new();
        for (k, v) in [
            ("bead_id", format!("sp-{i:06}")),
            ("state", if live { "READY" } else { "LANDED" }.to_string()),
            ("holds", "[]".to_string()),
            ("since", crate::db::now_epoch().to_string()),
            ("updated_at", crate::db::now_epoch().to_string()),
            ("priority", "2".to_string()),
            ("title", format!("a bead title of ordinary length number {i}")),
        ] {
            o.insert(k.into(), Value::String(v));
        }
        Value::Object(o)
    }

    #[test]
    fn ops_live_at_production_size_answers_under_the_ceiling_while_memory_is_unloaded() {
        let mut conn = Conn::unreachable();
        conn.enable_live();
        let live = conn.live.as_ref().unwrap();
        live.replace(vec![(0..7500).map(production_sized_bead).collect(), vec![], vec![]]).unwrap();
        views_tick(&conn);
        live.invalidate();
        assert!(conn.live().is_none(), "memory is unloaded, so only the kept answer can serve");
        let mut took: Vec<u128> = (0..100)
            .map(|_| {
                let t = std::time::Instant::now();
                let (code, out) = cmd_ops_view(&["ops_live".to_string()], &conn);
                assert_eq!(code, 0, "{out}");
                assert_eq!(serde_json::from_str::<Vec<Value>>(&out).unwrap().len(), 150);
                t.elapsed().as_millis()
            })
            .collect();
        took.sort_unstable();
        assert!(took[94] < 100, "p95 {} ms leaves no headroom under the 500 ms ceiling", took[94]);
    }
}
