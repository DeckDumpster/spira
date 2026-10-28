//! MENDING rows: opened by the batch's `Failed` event in the same transaction, then driven by
//! `mending pickup|event|sweep|list`. Every timestamp is stamped here from the clock, never
//! taken from the caller, so a mender cannot move its own deadline; `sweep` is how the
//! machine applies `Expire` to an overdue row.

use lifecycle::mending::{self, MendFacts, MendingEvent, MendingEventKind, MendingRow, MendingState, MEND_CAP_PER_BEAD};
use lifecycle::Refusal;
use serde_json::Value;

use crate::db::{CascadeStep, Conn, DbError, EventRecord};
use crate::rows::escape;
use crate::{flag, CANNOT_TELL, REFUSED};

const COLUMNS: &str = "batch_id, pass, suite, state, picked_at, deadline, triage_ended_at, diagnosis, awaiting_ask, version";

fn cell(row: &Value, col: &str) -> Option<String> {
    match row.get(col) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn num(row: &Value, col: &str) -> Option<i64> {
    cell(row, col).and_then(|s| s.parse().ok())
}

fn from_json(row: &Value) -> Result<MendingRow, DbError> {
    let state = MendingState::from_str(&cell(row, "state").unwrap_or_default())
        .ok_or_else(|| DbError::CannotTell(format!("bad state in mending row: {row}")))?;
    Ok(MendingRow {
        batch_id: cell(row, "batch_id").unwrap_or_default(),
        pass: num(row, "pass").unwrap_or(0) as u32,
        suite: cell(row, "suite").unwrap_or_default(),
        state,
        picked_at: num(row, "picked_at"),
        deadline: num(row, "deadline"),
        triage_ended_at: num(row, "triage_ended_at"),
        diagnosis: cell(row, "diagnosis"),
        awaiting_ask: cell(row, "awaiting_ask"),
        version: num(row, "version").unwrap_or(0) as u64,
    })
}

pub fn fetch(conn: &Conn, batch: &str, pass: u32, suite: &str) -> Result<Option<MendingRow>, DbError> {
    let rows = conn.query(&format!(
        "SELECT {COLUMNS} FROM mending WHERE batch_id = '{}' AND pass = {pass} AND suite = '{}'",
        escape(batch),
        escape(suite)
    ))?;
    rows.first().map(from_json).transpose()
}

fn opt_str(v: &Option<String>) -> String {
    v.as_ref().map_or("NULL".to_string(), |s| format!("'{}'", escape(s)))
}

fn opt_num(v: Option<i64>) -> String {
    v.map_or("NULL".to_string(), |n| n.to_string())
}

fn set_clause(r: &MendingRow) -> String {
    format!(
        "state = '{}', picked_at = {}, deadline = {}, triage_ended_at = {}, diagnosis = {}, awaiting_ask = {}, version = {}",
        r.state.as_str(),
        opt_num(r.picked_at),
        opt_num(r.deadline),
        opt_num(r.triage_ended_at),
        opt_str(&r.diagnosis),
        opt_str(&r.awaiting_ask),
        r.version
    )
}

fn kind_name(kind: &MendingEventKind) -> String {
    let v = serde_json::to_value(kind).unwrap_or(Value::Null);
    v.as_object().and_then(|m| m.keys().next()).cloned().unwrap_or_else(|| "unknown".into())
}

/// The batch's `Failed` step plus the MENDING row it opens, in one transaction. The row is
/// inserted only while the batch is still at `version`, so a stale event opens nothing.
pub fn open_with_failed(conn: &Conn, batch_step: CascadeStep, suite: &str, pass: u32, at: i64) -> Result<bool, DbError> {
    let batch = &batch_step.key;
    let preamble = format!(
        "INSERT INTO mending (batch_id, pass, suite, state, failed_at, version) \
         SELECT '{b}', {pass}, '{s}', '{state}', {at}, 0 FROM batch WHERE batch_id = '{b}' AND version = {v};\n",
        b = escape(batch),
        s = escape(suite),
        state = MendingState::Waiting.as_str(),
        v = batch_step.old_version,
    );
    Ok(conn.cascade(&preamble, &[batch_step])?.first().copied().unwrap_or(false))
}

pub fn dispatch(args: &[String], conn: &Conn) -> (i32, String) {
    let result = match args.first().map(String::as_str) {
        Some("pickup") => pickup(&args[1..], conn),
        Some("event") => event(&args[1..], conn),
        Some("sweep") => sweep(&args[1..], conn),
        Some("list") => list(&args[1..], conn),
        Some("close-ended") => close_ended(&args[1..], conn),
        Some("mend") => mend(&args[1..], conn),
        Some("tips") => tips(&args[1..], conn),
        _ => Err((CANNOT_TELL, "usage: spira-lc mending pickup <batch> --pass N --suite S --deadline-secs D --actor A | event <batch> --pass N --suite S --kind <json> --actor A | mend <batch> --pass N --suite S --members bead:from:to,... --facts <json> --fences <json> --actor A | tips --beads a,b | sweep --actor A [--deadline-secs D] | close-ended --actor A [--batch B] | list [--batch B] [--live]".to_string())),
    };
    match result {
        Ok(out) => (0, out),
        Err(e) => e,
    }
}

type Reply = Result<String, (i32, String)>;

fn cannot(e: DbError) -> (i32, String) {
    (CANNOT_TELL, format!("cannot tell: {e:?}"))
}

fn target(args: &[String], conn: &Conn) -> Result<MendingRow, (i32, String)> {
    let (Some(batch), Some(pass), Some(suite)) = (args.first(), flag(args, "--pass").and_then(|p| p.parse::<u32>().ok()), flag(args, "--suite")) else {
        return Err((CANNOT_TELL, "mending: <batch>, --pass N and --suite S are required".into()));
    };
    fetch(conn, batch, pass, &suite).map_err(cannot)?.ok_or((CANNOT_TELL, format!("mending: no row for {batch} pass {pass} {suite}")))
}

fn pickup(args: &[String], conn: &Conn) -> Reply {
    let row = target(args, conn)?;
    let deadline_s = flag(args, "--deadline-secs")
        .and_then(|d| d.parse::<u64>().ok())
        .ok_or((CANNOT_TELL, "mending pickup: --deadline-secs N is required".to_string()))?;
    let actor = flag(args, "--actor").ok_or((CANNOT_TELL, "mending: --actor is required".to_string()))?;
    apply(conn, &row, MendingEventKind::Pickup { at: crate::db::now_epoch(), deadline_s }, &actor)
}

fn event(args: &[String], conn: &Conn) -> Reply {
    let row = target(args, conn)?;
    let actor = flag(args, "--actor").ok_or((CANNOT_TELL, "mending: --actor is required".to_string()))?;
    let mut kind: Value = flag(args, "--kind")
        .and_then(|k| serde_json::from_str(&k).ok())
        .ok_or((CANNOT_TELL, "mending event: --kind must be a JSON mending event".to_string()))?;
    if let Some(body) = kind.as_object_mut().and_then(|m| m.values_mut().next()).and_then(Value::as_object_mut) {
        if body.contains_key("at") {
            body.insert("at".into(), Value::from(crate::db::now_epoch()));
        }
    }
    let kind: MendingEventKind = serde_json::from_value(kind).map_err(|e| (CANNOT_TELL, format!("mending event: --kind did not parse: {e}")))?;
    if matches!(kind, MendingEventKind::Pickup { .. } | MendingEventKind::Expire { .. } | MendingEventKind::Resume { .. }) {
        return Err((REFUSED, "mending event: pickup, resume and expire are the machine's own, not a mender's".into()));
    }
    if matches!(kind, MendingEventKind::Mended { .. }) {
        return Err((REFUSED, "mending event: a mend goes through `mending mend`, which assesses the tip".into()));
    }
    apply(conn, &row, kind, &actor)
}

fn apply(conn: &Conn, row: &MendingRow, kind: MendingEventKind, actor: &str) -> Reply {
    let ev = MendingEvent { expect: row.state, version: row.version, kind: kind.clone(), actor: actor.to_string() };
    let outcome = mending::apply(row, &ev);
    let at = crate::db::now_epoch();
    let rec = EventRecord::of_apply(
        "mending",
        row.key(),
        kind_name(&kind),
        row.state.as_str(),
        row.state.as_str(),
        outcome.refusal.as_ref().map(crate::refusal_name),
        serde_json::to_value(&kind).unwrap_or(Value::Null),
        actor,
        at,
    );
    if !outcome.applied {
        conn.insert_refusal_event(&rec).map_err(cannot)?;
        return Err((REFUSED, format!("refused: {:?}", outcome.refusal)));
    }
    match conn.cas_update_and_log("mending", "CONCAT(batch_id, '#', pass, '#', suite)", &row.key(), row.version, &set_clause(&outcome.row), &rec, outcome.row.state.as_str()) {
        Ok(true) => Ok(outcome.row.state.as_str().to_string()),
        Ok(false) => Err((REFUSED, "refused: lost the race to another writer".into())),
        Err(e) => Err(cannot(e)),
    }
}

fn sweep(args: &[String], conn: &Conn) -> Reply {
    let actor = flag(args, "--actor").ok_or((CANNOT_TELL, "mending sweep: --actor is required".to_string()))?;
    if let Some(deadline_s) = flag(args, "--deadline-secs").and_then(|d| d.parse::<u64>().ok()) {
        resume_answered(conn, deadline_s, &actor).map_err(cannot)?;
    }
    Ok(expire_overdue(conn, &actor).map_err(cannot)?.to_string())
}

fn ask_open(conn: &Conn, ask: &str) -> Result<bool, DbError> {
    Ok(crate::rows::fetch_ask(conn, ask)?.is_some_and(|a| !a.state.is_terminal()))
}

/// Gives every MENDING row whose awaited ask has closed a fresh deadline to record its mend.
pub fn resume_answered(conn: &Conn, deadline_s: u64, actor: &str) -> Result<usize, DbError> {
    let rows = conn.query(&format!("SELECT {COLUMNS} FROM mending WHERE state = 'MENDING' AND awaiting_ask IS NOT NULL"))?;
    let mut resumed = 0;
    for row in rows.iter().map(from_json) {
        let row = row?;
        let Some(ask) = row.awaiting_ask.clone() else { continue };
        if !ask_open(conn, &ask)? && apply(conn, &row, MendingEventKind::Resume { at: crate::db::now_epoch(), deadline_s }, actor).is_ok() {
            resumed += 1;
        }
    }
    Ok(resumed)
}

/// Applies `Expire` to every MENDING row whose deadline has passed; returns how many applied.
pub fn expire_overdue(conn: &Conn, actor: &str) -> Result<usize, DbError> {
    let now = crate::db::now_epoch();
    let rows = conn.query(&format!("SELECT {COLUMNS} FROM mending WHERE state = 'MENDING' AND awaiting_ask IS NULL AND deadline <= {now}"))?;
    let mut applied = 0;
    for row in rows.iter().map(from_json) {
        let row = row?;
        if apply(conn, &row, MendingEventKind::Expire { at: now }, actor).is_ok() {
            applied += 1;
        }
    }
    Ok(applied)
}

const BATCH_ENDED: &str = "SELECT batch_id FROM batch WHERE state IN ('LANDED', 'SETTLED', 'ABANDONED', 'DISCARDED')";

fn close_ended(args: &[String], conn: &Conn) -> Reply {
    let actor = flag(args, "--actor").ok_or((CANNOT_TELL, "mending close-ended: --actor is required".to_string()))?;
    let scope = flag(args, "--batch").map_or(String::new(), |b| format!(" AND batch_id = '{}'", escape(&b)));
    let rows = conn
        .query(&format!("SELECT {COLUMNS} FROM mending WHERE state IN ('WAITING', 'MENDING') AND batch_id IN ({BATCH_ENDED}){scope}"))
        .map_err(cannot)?;
    let mut closed = 0;
    for row in rows.iter().map(from_json) {
        let mut row = row.map_err(cannot)?;
        let now = crate::db::now_epoch();
        if row.overdue(now) {
            closed += usize::from(apply(conn, &row, MendingEventKind::Expire { at: now }, &actor).is_ok());
            continue;
        }
        if row.triage_ended_at.is_none() && apply(conn, &row, MendingEventKind::TriageEnded { at: now }, &actor).is_ok() {
            row = fetch(conn, &row.batch_id, row.pass, &row.suite).map_err(cannot)?.unwrap_or(row);
        }
        let why = "the batch closed; the failure ended with it";
        let diagnosis = row.diagnosis.as_ref().map_or(why.to_string(), |d| format!("{d}\n{why}"));
        closed += usize::from(apply(conn, &row, MendingEventKind::Return { at: now, diagnosis }, &actor).is_ok());
    }
    Ok(closed.to_string())
}

fn list(args: &[String], conn: &Conn) -> Reply {
    let mut wheres = vec!["1 = 1".to_string()];
    if let Some(b) = flag(args, "--batch") {
        wheres.push(format!("batch_id = '{}'", escape(&b)));
    }
    if args.iter().any(|a| a == "--live") {
        wheres.push("state IN ('WAITING', 'MENDING')".into());
    }
    let rows = conn.query(&format!("SELECT {COLUMNS}, failed_at FROM mending WHERE {} ORDER BY failed_at, batch_id, pass, suite", wheres.join(" AND "))).map_err(cannot)?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            let m = from_json(r).map_err(cannot)?;
            let mut v = serde_json::to_value(&m).unwrap_or(Value::Null);
            v["state"] = Value::from(m.state.as_str());
            v["failed_at"] = num(r, "failed_at").map_or(Value::Null, Value::from);
            let waiting = match &m.awaiting_ask {
                Some(a) => ask_open(conn, a).map_err(cannot)?,
                None => false,
            };
            v["ask_open"] = Value::from(waiting);
            Ok(v)
        })
        .collect::<Result<_, (i32, String)>>()?;
    Ok(Value::Array(out).to_string())
}

const MENDING_KEY: &str = "CONCAT(batch_id, '#', pass, '#', suite)";

/// One member's branch moving from `from` to `to`.
struct Move {
    bead: String,
    from: String,
    to: String,
}

fn moves(spec: &str) -> Option<Vec<Move>> {
    let v: Vec<Move> = spec
        .split(',')
        .filter(|m| !m.is_empty())
        .map(|m| {
            let mut p = m.split(':');
            Some(Move { bead: p.next()?.to_string(), from: p.next()?.to_string(), to: p.next()?.to_string() })
        })
        .collect::<Option<_>>()?;
    (!v.is_empty()).then_some(v)
}

fn mends_of(conn: &Conn, bead: &str) -> Result<Vec<Value>, DbError> {
    conn.query(&format!("SELECT tip, diagnosis FROM mend WHERE bead_id = '{}' ORDER BY seq", escape(bead)))
}

/// The refusal is logged on the row's log; the row itself stays as it was.
fn refuse_logged(conn: &Conn, row: &MendingRow, refusal: Refusal, actor: &str) -> (i32, String) {
    let rec = EventRecord::of_apply(
        "mending",
        row.key(),
        "Mended",
        row.state.as_str(),
        row.state.as_str(),
        Some(crate::refusal_name(&refusal)),
        Value::String(format!("{refusal:?}")),
        actor,
        crate::db::now_epoch(),
    );
    if let Err(e) = conn.insert_refusal_event(&rec) {
        return cannot(e);
    }
    (REFUSED, format!("refused: {refusal:?}"))
}

/// A mend the machine will not take returns the failure's row with the reason as its diagnosis.
fn return_with(conn: &Conn, row: &MendingRow, why: &str, actor: &str) -> Reply {
    let diagnosis = match &row.diagnosis {
        Some(d) => format!("{d}\n{why}"),
        None => why.to_string(),
    };
    apply(conn, row, MendingEventKind::Return { at: crate::db::now_epoch(), diagnosis }, actor)
}

fn mend(args: &[String], conn: &Conn) -> Reply {
    let row = target(args, conn)?;
    let actor = flag(args, "--actor").ok_or((CANNOT_TELL, "mending: --actor is required".to_string()))?;
    let members = flag(args, "--members").and_then(|m| moves(&m)).ok_or((CANNOT_TELL, "mending mend: --members bead:from:to,... is required".to_string()))?;
    let facts: MendFacts = flag(args, "--facts")
        .and_then(|f| serde_json::from_str(&f).ok())
        .ok_or((CANNOT_TELL, "mending mend: --facts must be the mend's JSON facts".to_string()))?;
    let fences: Vec<String> = flag(args, "--fences")
        .and_then(|f| serde_json::from_str(&f).ok())
        .ok_or((CANNOT_TELL, "mending mend: --fences must be a JSON list of fence paths".to_string()))?;
    if row.state != MendingState::Mending && !(row.state == MendingState::Waiting && mending::may_settle_unpicked(&actor)) {
        return Err(refuse_logged(conn, &row, Refusal::IllegalTransition { state: row.state.as_str().into(), event: "Mended".into() }, &actor));
    }
    for m in &members {
        let prior = mends_of(conn, &m.bead).map_err(cannot)?;
        if prior.len() >= MEND_CAP_PER_BEAD {
            let diagnoses: Vec<String> = prior.iter().filter_map(|p| cell(p, "diagnosis")).collect();
            let why = format!("mend cap: {} was already mended {} times; diagnoses: {}", m.bead, prior.len(), diagnoses.join(" | "));
            return_with(conn, &row, &why, &actor)?;
            return Err((REFUSED, format!("refused: {:?}; the failure is returned", Refusal::MendCap { bead: m.bead.clone(), mends: prior.len() })));
        }
        if let Some(current) = prior.last().and_then(|p| cell(p, "tip")) {
            if current != m.from {
                let why = format!("fix no longer applies: {} moved to {current} under this mend, which was built on {}", m.bead, m.from);
                return_with(conn, &row, &why, &actor)?;
                return Err((REFUSED, format!("refused: {:?}; the failure is returned", Refusal::StaleTip { bead: m.bead.clone(), current, given: m.from.clone() })));
            }
        }
    }
    if let Some(ask) = facts.ask.as_deref().filter(|a| !a.is_empty()) {
        if crate::rows::fetch_ask(conn, ask).map_err(cannot)?.is_none() {
            return Err(refuse_logged(conn, &row, Refusal::MendRefused { reasons: vec![format!("ask {ask} does not exist")] }, &actor));
        }
    }
    if let Err(refusal) = mending::assess(&facts, &fences) {
        return Err(refuse_logged(conn, &row, refusal, &actor));
    }
    let at = crate::db::now_epoch();
    let kind = MendingEventKind::Mended { at };
    let ev = MendingEvent { expect: row.state, version: row.version, kind: kind.clone(), actor: actor.clone() };
    let outcome = mending::apply(&row, &ev);
    if !outcome.applied {
        return Err(refuse_logged(conn, &row, outcome.refusal.unwrap_or(Refusal::IllegalTransition { state: row.state.as_str().into(), event: "Mended".into() }), &actor));
    }
    let evidence = serde_json::json!({"Mended": {"at": at, "members": members.iter().map(|m| format!("{}:{}:{}", m.bead, m.from, m.to)).collect::<Vec<_>>(), "ask": facts.ask, "checked": facts.checked}});
    let rec = EventRecord::of_apply("mending", row.key(), "Mended", row.state.as_str(), row.state.as_str(), None, evidence, &actor, at);
    let checked = facts.checked.join(",");
    let inserts: String = members
        .iter()
        .map(|m| {
            format!(
                "INSERT INTO mend (batch_id, pass, suite, bead_id, from_tip, tip, ask_id, checked, diagnosis, at) \
                 SELECT batch_id, pass, suite, '{bead}', '{from}', '{to}', {ask}, '{checked}', {diag}, {at} FROM mending \
                 WHERE batch_id = '{b}' AND pass = {pass} AND suite = '{s}' AND version = {v};\n",
                bead = escape(&m.bead),
                from = escape(&m.from),
                to = escape(&m.to),
                ask = opt_str(&facts.ask),
                checked = escape(&checked),
                diag = opt_str(&row.diagnosis),
                b = escape(&row.batch_id),
                pass = row.pass,
                s = escape(&row.suite),
                v = row.version,
            )
        })
        .collect();
    let step = CascadeStep {
        table: "mending",
        key_column: MENDING_KEY,
        key: row.key(),
        old_version: row.version,
        set_clause: set_clause(&outcome.row),
        applied_to_state: outcome.row.state.as_str().to_string(),
        event: rec,
    };
    match conn.cascade(&inserts, &[step]).map_err(cannot)?.first().copied().unwrap_or(false) {
        true => Ok(outcome.row.state.as_str().to_string()),
        false => Err((REFUSED, "refused: lost the race to another writer".into())),
    }
}

/// Each bead's current mended tip, or null when it has none: what a second mender must build on.
fn tips(args: &[String], conn: &Conn) -> Reply {
    let beads = flag(args, "--beads").ok_or((CANNOT_TELL, "mending tips: --beads a,b is required".to_string()))?;
    let mut out = serde_json::Map::new();
    for bead in beads.split(',').filter(|b| !b.is_empty()) {
        let tip = mends_of(conn, bead).map_err(cannot)?.last().and_then(|p| cell(p, "tip"));
        out.insert(bead.to_string(), tip.map_or(Value::Null, Value::from));
    }
    Ok(Value::Object(out).to_string())
}
