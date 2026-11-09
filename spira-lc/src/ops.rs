//! The ops read model (`lifecycle/migrations/0007-ops-read-model.sql`, `0009-where-stuck.sql`):
//! `ops-view <view>` returns one view as a JSON array and `ops-graph` the machine's legal edges.
//! Both ride `dispatch`, so a refresh through `spira-lc serve` is one query on the held connection.

use serde_json::Value;

use crate::db::Conn;

/// The only views `ops-view` will name; the argument is matched, never interpolated.
pub const VIEWS: [&str; 6] = ["ops_live", "ops_round", "ops_recent", "ops_edges", "ops_dwell", "ops_dwell_p95"];

/// The machines `ops-gantt` reads events of, and the machines `ops-graph` draws.
pub const GANTT_MACHINES: [&str; 2] = ["bead", "batch"];
pub const GRAPH_MACHINES: [&str; 4] = ["bead", "batch", "ask", "mending"];

/// Every applied state change of the last day, read from the event time index. `{machine}` is
/// replaced only by a name from [`GANTT_MACHINES`].
pub const GANTT_SQL: &str = "SELECT seq, lc_key AS bead_id, event, from_state, to_state, at FROM event \
     WHERE machine = '{machine}' AND applied = 1 AND at >= UNIX_TIMESTAMP() - 86400 AND from_state <> to_state";

fn gantt_sql(machine: &'static str) -> String {
    GANTT_SQL.replace("{machine}", machine)
}

/// The `--machine` named in `args`, matched against `allowed`; `default` when the flag is absent.
fn machine_arg(verb: &str, args: &[String], allowed: &[&'static str]) -> Result<&'static str, (i32, String)> {
    let Some(at) = args.iter().position(|a| a == "--machine") else {
        return Ok(allowed[0]);
    };
    args.get(at + 1)
        .and_then(|m| allowed.iter().find(|a| **a == m.as_str()))
        .copied()
        .ok_or_else(|| (2, format!("{verb}: --machine takes one of {}", allowed.join(" | "))))
}

/// Terminal beads of the last day, one index range per state.
fn terminal_sql() -> String {
    let states = lifecycle::bead::BeadState::TERMINAL.iter().map(|s| format!("'{}'", s.as_str())).collect::<Vec<_>>().join(", ");
    format!("SELECT bead_id, state, since, updated_at FROM bead WHERE state IN ({states}) AND since >= UNIX_TIMESTAMP() - 86400")
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
    out.insert("graph".into(), serde_json::from_str(&graph_json("bead")).unwrap_or(Value::Null));
    for (key, sql) in &queries {
        match conn.query(sql) {
            Ok(rows) => out.insert((*key).into(), Value::Array(rows)),
            Err(e) => return (2, format!("cannot tell: {key}: {e:?}")),
        };
    }
    (0, Value::Object(out).to_string())
}

fn view_arg(args: &[String]) -> Result<&'static str, (i32, String)> {
    args.first().and_then(|a| VIEWS.iter().find(|v| **v == a.as_str())).copied().ok_or_else(|| (2, format!("ops-view: takes one of {}", VIEWS.join(" | "))))
}

/// `ops-view <view>`: one view as a JSON array.
pub fn cmd_ops_view(args: &[String], conn: &Conn) -> (i32, String) {
    let view = match view_arg(args) {
        Ok(v) => v,
        Err(refusal) => return refusal,
    };
    match conn.query(&format!("SELECT * FROM {view}")) {
        Ok(rows) => (0, Value::Array(rows).to_string()),
        Err(e) => (2, format!("cannot tell: {e:?}")),
    }
}

/// The stuck page's bars: the day's transitions plus the clock they are measured against.
pub fn cmd_ops_gantt(args: &[String], conn: &Conn) -> (i32, String) {
    let machine = match machine_arg("ops-gantt", args, &GANTT_MACHINES) {
        Ok(m) => m,
        Err(refusal) => return refusal,
    };
    if args.iter().any(|a| a == "--print-sql") {
        return (0, gantt_sql(machine));
    }
    match conn.query(&gantt_sql(machine)) {
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
    let bead = conn.query(&format!("SELECT bead_id, state, holder, persona, lease_until, holds, reason, gate_key, updated_at FROM bead WHERE bead_id = '{key}'"));
    let events = conn.query(&format!("SELECT seq, event, from_state, to_state, applied, refusal, evidence, actor, at FROM event WHERE machine = 'bead' AND lc_key = '{key}' ORDER BY seq"));
    match (bead, events) {
        (Ok(b), Ok(e)) if b.is_empty() && e.is_empty() => (1, "{}".into()),
        (Ok(b), Ok(e)) => (0, serde_json::json!({ "now": crate::db::now_epoch(), "bead": b.first(), "events": e }).to_string()),
        (Err(e), _) | (_, Err(e)) => (2, format!("cannot tell: {e:?}")),
    }
}

/// A machine's legal moves, read from its own transition table; the bead machine's with no flag.
pub fn cmd_ops_graph(args: &[String], _conn: &Conn) -> (i32, String) {
    match machine_arg("ops-graph", args, &GRAPH_MACHINES) {
        Ok(machine) => (0, graph_json(machine)),
        Err(refusal) => refusal,
    }
}

/// States spelled as the machine's state column holds them, so a pane joins edges to view rows.
fn graph_json(machine: &str) -> String {
    macro_rules! edges_of {
        ($m:ident) => {
            lifecycle::$m::legal_edges().iter().map(|e| serde_json::json!({"from": e.from.as_str(), "event": e.event, "to": e.to.as_str()})).collect::<Vec<Value>>()
        };
    }
    let edges = match machine {
        "batch" => edges_of!(batch),
        "ask" => edges_of!(ask),
        "mending" => edges_of!(mending),
        _ => edges_of!(bead),
    };
    Value::Array(edges).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refusal_query_reads_only_refused_bead_events_of_the_window() {
        let q = refusals_sql(86400);
        assert!(q.contains("machine = 'bead' AND applied = 0") && q.contains("UNIX_TIMESTAMP() - 86400"), "{q}");
        assert!(q.contains("GROUP BY actor, event, from_state, refusal"), "{q}");
    }

    #[test]
    fn the_graph_is_json_edges_with_state_names_and_events() {
        let out = graph_json("bead");
        let edges: Vec<Value> = serde_json::from_str(&out).unwrap();
        let has = |f: &str, ev: &str, t: &str| edges.iter().any(|e| e["from"] == f && e["event"] == ev && e["to"] == t);
        assert!(has("READY", "Claim", "WORKING"), "{out}");
        assert!(has("SUBMITTED", "GatePass", "CERTIFIED"), "{out}");
        assert!(has("CERTIFIED", "GateRed", "REWORK"), "{out}");
        let terminal = ["LANDED", "DROPPED", "SUPERSEDED", "DONE"];
        assert!(!edges.iter().any(|e| terminal.iter().any(|t| e["from"] == *t)), "no exit from a terminal state: {out}");
    }

    #[test]
    fn ops_graph_machine_answers_that_machines_edges() {
        let edges = |args: &[&str]| -> Vec<Value> {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            let (rc, out) = cmd_ops_graph(&args, &Conn::unreachable());
            assert_eq!(rc, 0, "{out}");
            serde_json::from_str(&out).unwrap()
        };
        let has = |edges: &[Value], f: &str, ev: &str, t: &str| edges.iter().any(|e| e["from"] == f && e["event"] == ev && e["to"] == t);
        let batch = edges(&["--machine", "batch"]);
        assert!(has(&batch, "OPEN", "Abandon", "ABANDONED") && has(&batch, "GREEN", "FastForward", "LANDED"));
        assert!(has(&edges(&["--machine", "ask"]), "OPEN", "Answer", "ANSWERED"));
        assert!(has(&edges(&["--machine", "mending"]), "WAITING", "Pickup", "MENDING"));
        assert_eq!(edges(&[]), edges(&["--machine", "bead"]), "no flag answers as the bead machine");
        let (rc, out) = cmd_ops_graph(&["--machine".into(), "delivery".into()], &Conn::unreachable());
        assert_eq!(rc, 2, "{out}");
    }

    #[test]
    fn the_gantt_query_selects_the_event_and_filters_by_the_machine_asked_for() {
        assert!(gantt_sql("bead").contains("event, from_state") && gantt_sql("bead").contains("machine = 'bead'"));
        let print = |args: &[&str]| cmd_ops_gantt(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>(), &Conn::unreachable());
        assert!(print(&["--print-sql"]).1.contains("machine = 'bead'"), "no flag reads the bead machine");
        assert!(print(&["--machine", "batch", "--print-sql"]).1.contains("machine = 'batch'"));
        let (rc, out) = print(&["--machine", "bead' OR 1=1 --", "--print-sql"]);
        assert_eq!(rc, 2, "{out}");
        assert_eq!(print(&["--machine", "ask", "--print-sql"]).0, 2, "only machines the event log carries bead facts for");
    }

    #[test]
    fn the_gantt_read_is_covered_by_one_event_index_so_a_day_of_transitions_touches_no_table_row() {
        let migrations = concat!(env!("CARGO_MANIFEST_DIR"), "/../lifecycle/migrations");
        let indexes: Vec<String> = std::fs::read_dir(migrations)
            .unwrap()
            .flat_map(|f| std::fs::read_to_string(f.unwrap().path()).unwrap().lines().map(String::from).collect::<Vec<_>>())
            .filter(|l| l.starts_with("CREATE INDEX") && l.contains(" ON event ("))
            .collect();
        let selected = ["lc_key", "event", "from_state", "to_state", "at"];
        let sql = gantt_sql("bead");
        assert!(selected.iter().all(|c| sql.contains(c)), "{sql}");
        let covers = |ix: &String| selected.iter().all(|c| ix.contains(&format!("{c},")) || ix.contains(&format!("{c})")));
        assert!(indexes.iter().any(|ix| covers(ix) && ix.contains("(machine, applied, at,")), "no event index covers the gantt read: {indexes:?}");
    }

    #[test]
    fn ops_view_refuses_anything_but_its_views_before_any_sql() {
        for v in VIEWS {
            assert_eq!(view_arg(&[v.to_string()]), Ok(v));
        }
        assert_eq!(view_arg(&["bead".to_string()]).unwrap_err().0, 2);
        assert_eq!(view_arg(&[]).unwrap_err().0, 2);
    }
}
