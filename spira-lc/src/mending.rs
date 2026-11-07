//! MENDING rows: opened by the batch's `Failed` event in the same transaction, then driven by
//! `mending pickup|event|sweep|list`. Every timestamp is stamped here from the clock, never
//! taken from the caller, so a mender cannot move its own deadline; `sweep` is how the
//! machine applies `Expire` to an overdue row.

use lifecycle::mending::{self, MendFacts, MendingEvent, MendingEventKind, MendingRow, MendingState, MEND_CAP_PER_BEAD};
use lifecycle::batch::{self, BatchEvent, BatchEventKind, BatchRow, BatchState};
use lifecycle::Refusal;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

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

pub(crate) fn from_json(row: &Value) -> Result<MendingRow, DbError> {
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
pub(crate) fn open_with_failed(conn: &Conn, batch_step: CascadeStep, opens: &MendingRow, at: i64) -> Result<bool, DbError> {
    let batch = &batch_step.key;
    let (suite, pass) = (&opens.suite, opens.pass);
    let preamble = format!(
        "INSERT INTO mending (batch_id, pass, suite, state, failed_at, version) \
         SELECT '{b}', {pass}, '{s}', '{state}', {at}, 0 FROM batch WHERE batch_id = '{b}' AND version = {v};\n",
        b = escape(batch),
        s = escape(suite),
        state = opens.state.as_str(),
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

const BATCH_MAX: usize = 64;
const SUITE_MAX: usize = 160;
const KEY_MAX: usize = 256;

/// Refuse an over-long field by name before it reaches SQL, mirroring `mending` and `event.lc_key`.
pub fn check_widths(batch: &str, pass: u32, suite: &str) -> Result<(), (i32, String)> {
    let key = lifecycle::mending::key(batch, pass, suite);
    for (name, value, max) in [("batch_id", batch, BATCH_MAX), ("suite", suite, SUITE_MAX), ("lc_key", key.as_str(), KEY_MAX)] {
        let n = value.chars().count();
        if n > max {
            return Err((CANNOT_TELL, format!("mending: refused: {name} is {n} chars (max {max}); {name}={value}")));
        }
    }
    Ok(())
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum FailedPlan {
    /// The suite already failed in this pass: no event, no version move.
    Recorded(String),
    /// The batch's `Failed` outcome and log entry; an applied one opens `opens` in the same txn.
    Planned { outcome: lifecycle::Outcome<BatchRow>, record: EventRecord, opens: MendingRow },
}

pub(crate) fn plan_failed(row: &BatchRow, asked: (&str, BatchState, u64), actor: &str, kind: &BatchEventKind, recorded: Option<&MendingRow>, now: i64) -> FailedPlan {
    let BatchEventKind::Failed { suite, n } = kind else { unreachable!("plan_failed takes a Failed event") };
    if recorded.is_some() {
        return FailedPlan::Recorded(format!("already recorded: {suite} failed in pass {n}"));
    }
    let (expect_name, expect, version) = asked;
    let outcome = batch::apply(row, &BatchEvent { expect, version, kind: kind.clone(), actor: actor.to_string() });
    let evidence = serde_json::to_value(kind).unwrap_or(Value::Null);
    let record = EventRecord::of_apply("batch", &row.batch_id, crate::kind_name(&evidence), expect_name, row.state.as_str(), outcome.refusal.as_ref().map(crate::refusal_name), evidence, actor, now);
    FailedPlan::Planned { outcome, record, opens: MendingRow::failed(&row.batch_id, *n, suite) }
}

pub(crate) fn parse_target(args: &[String]) -> Result<(String, u32, String), (i32, String)> {
    let (Some(batch), Some(pass), Some(suite)) = (args.first(), flag(args, "--pass").and_then(|p| p.parse::<u32>().ok()), flag(args, "--suite")) else {
        return Err((CANNOT_TELL, "mending: <batch>, --pass N and --suite S are required".into()));
    };
    check_widths(batch, pass, &suite)?;
    Ok((batch.clone(), pass, suite))
}

fn target(args: &[String], conn: &Conn) -> Result<MendingRow, (i32, String)> {
    let (batch, pass, suite) = parse_target(args)?;
    fetch(conn, &batch, pass, &suite).map_err(cannot)?.ok_or((CANNOT_TELL, format!("mending: no row for {batch} pass {pass} {suite}")))
}

/// One planned event: the row before and after, what the machine decided, and the log entry.
#[derive(Debug, Clone)]
pub(crate) struct Step {
    pub before: MendingRow,
    pub after: MendingRow,
    pub event: MendingEvent,
    pub record: EventRecord,
    pub refusal: Option<Refusal>,
}

fn log_of(row: &MendingRow, event: &str, refusal: Option<String>, evidence: Value, actor: &str, now: i64) -> EventRecord {
    EventRecord::of_apply("mending", row.key(), event, row.state.as_str(), row.state.as_str(), refusal, evidence, actor, now)
}

pub(crate) fn plan_step(row: &MendingRow, kind: MendingEventKind, actor: &str, now: i64) -> Step {
    let event = MendingEvent { expect: row.state, version: row.version, kind: kind.clone(), actor: actor.to_string() };
    let outcome = mending::apply(row, &event);
    let evidence = serde_json::to_value(&kind).unwrap_or(Value::Null);
    let record = log_of(row, &kind_name(&kind), outcome.refusal.as_ref().map(crate::refusal_name), evidence, actor, now);
    Step { before: row.clone(), after: outcome.row, event, record, refusal: outcome.refusal }
}

fn plan_refused(row: &MendingRow, refusal: Refusal, actor: &str, now: i64) -> Step {
    let record = log_of(row, "Mended", Some(crate::refusal_name(&refusal)), Value::String(format!("{refusal:?}")), actor, now);
    let event = MendingEvent { expect: row.state, version: row.version, kind: MendingEventKind::Mended { at: now }, actor: actor.to_string() };
    Step { before: row.clone(), after: row.clone(), event, record, refusal: Some(refusal) }
}

/// Ok(true) when the step was applied; a refusal is logged and is Ok(false), as is a lost race.
fn persist(conn: &Conn, step: &Step) -> Result<bool, DbError> {
    if step.refusal.is_some() {
        conn.insert_refusal_event(&step.record)?;
        return Ok(false);
    }
    conn.cas_update_and_log("mending", MENDING_KEY, &step.before.key(), step.before.version, &set_clause(&step.after), &step.record, step.after.state.as_str())
}

fn commit(conn: &Conn, step: &Step) -> Reply {
    match persist(conn, step) {
        Ok(true) => Ok(step.after.state.as_str().to_string()),
        Ok(false) if step.refusal.is_some() => Err((REFUSED, format!("refused: {:?}", step.refusal))),
        Ok(false) => Err((REFUSED, "refused: lost the race to another writer".into())),
        Err(e) => Err(cannot(e)),
    }
}

fn apply(conn: &Conn, row: &MendingRow, kind: MendingEventKind, actor: &str) -> Reply {
    commit(conn, &plan_step(row, kind, actor, crate::db::now_epoch()))
}

pub(crate) fn plan_pickup(row: &MendingRow, deadline_s: u64, actor: &str, now: i64) -> Result<Vec<Step>, (i32, String)> {
    Ok(vec![plan_step(row, MendingEventKind::Pickup { at: now, deadline_s }, actor, now)])
}

pub(crate) fn plan_event(row: &MendingRow, kind: Option<&str>, actor: &str, now: i64) -> Result<Vec<Step>, (i32, String)> {
    let mut kind: Value = kind
        .and_then(|k| serde_json::from_str(k).ok())
        .ok_or((CANNOT_TELL, "mending event: --kind must be a JSON mending event".to_string()))?;
    if let Some(body) = kind.as_object_mut().and_then(|m| m.values_mut().next()).and_then(Value::as_object_mut) {
        if body.contains_key("at") {
            body.insert("at".into(), Value::from(now));
        }
    }
    let kind: MendingEventKind = serde_json::from_value(kind).map_err(|e| (CANNOT_TELL, format!("mending event: --kind did not parse: {e}")))?;
    if matches!(kind, MendingEventKind::Pickup { .. } | MendingEventKind::Expire { .. } | MendingEventKind::Resume { .. }) {
        return Err((REFUSED, "mending event: pickup, resume and expire are the machine's own, not a mender's".into()));
    }
    if matches!(kind, MendingEventKind::Mended { .. }) {
        return Err((REFUSED, "mending event: a mend goes through `mending mend`, which assesses the tip".into()));
    }
    Ok(vec![plan_step(row, kind, actor, now)])
}

fn actor_of(args: &[String]) -> Result<String, (i32, String)> {
    flag(args, "--actor").ok_or((CANNOT_TELL, "mending: --actor is required".to_string()))
}

fn run_steps(conn: &Conn, steps: Vec<Step>) -> Reply {
    let mut last = Ok(String::new());
    for step in &steps {
        last = commit(conn, step);
    }
    last
}

fn pickup(args: &[String], conn: &Conn) -> Reply {
    let row = target(args, conn)?;
    let deadline_s = flag(args, "--deadline-secs")
        .and_then(|d| d.parse::<u64>().ok())
        .ok_or((CANNOT_TELL, "mending pickup: --deadline-secs N is required".to_string()))?;
    let actor = actor_of(args)?;
    run_steps(conn, plan_pickup(&row, deadline_s, &actor, crate::db::now_epoch())?)
}

fn event(args: &[String], conn: &Conn) -> Reply {
    let row = target(args, conn)?;
    let actor = actor_of(args)?;
    run_steps(conn, plan_event(&row, flag(args, "--kind").as_deref(), &actor, crate::db::now_epoch())?)
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

/// `Expire` for every overdue row among `rows`.
pub(crate) fn plan_expire_overdue(rows: &[MendingRow], actor: &str, now: i64) -> Vec<Step> {
    rows.iter().filter(|r| r.overdue(now)).map(|r| plan_step(r, MendingEventKind::Expire { at: now }, actor, now)).collect()
}

/// Applies `Expire` to every MENDING row whose deadline has passed; returns how many applied.
pub fn expire_overdue(conn: &Conn, actor: &str) -> Result<usize, DbError> {
    let now = crate::db::now_epoch();
    let rows = conn.query(&format!("SELECT {COLUMNS} FROM mending WHERE state = 'MENDING' AND awaiting_ask IS NULL AND deadline <= {now}"))?;
    let rows = rows.iter().map(from_json).collect::<Result<Vec<_>, _>>()?;
    Ok(plan_expire_overdue(&rows, actor, now).iter().filter(|s| matches!(persist(conn, s), Ok(true))).count())
}

const BATCH_ENDED: &str = "SELECT batch_id FROM batch WHERE state IN ('LANDED', 'SETTLED', 'ABANDONED', 'DISCARDED')";

/// Ends each open row of a batch in `ended` (limited to `scope` when given): an overdue row
/// expires; any other has its triage stamped and is returned with the closing reason.
pub(crate) fn plan_close_ended(rows: &[MendingRow], ended: &BTreeSet<String>, scope: Option<&str>, actor: &str, now: i64) -> Vec<Step> {
    let mut steps = Vec::new();
    for row in rows.iter().filter(|r| !r.state.is_terminal() && ended.contains(&r.batch_id) && scope.is_none_or(|b| b == r.batch_id)) {
        if row.overdue(now) {
            steps.push(plan_step(row, MendingEventKind::Expire { at: now }, actor, now));
            continue;
        }
        let mut row = row.clone();
        if row.triage_ended_at.is_none() {
            let triage = plan_step(&row, MendingEventKind::TriageEnded { at: now }, actor, now);
            if triage.refusal.is_none() {
                row = triage.after.clone();
            }
            steps.push(triage);
        }
        let why = "the batch closed; the failure ended with it";
        let diagnosis = row.diagnosis.as_ref().map_or(why.to_string(), |d| format!("{d}\n{why}"));
        steps.push(plan_step(&row, MendingEventKind::Return { at: now, diagnosis }, actor, now));
    }
    steps
}

fn close_ended(args: &[String], conn: &Conn) -> Reply {
    let actor = flag(args, "--actor").ok_or((CANNOT_TELL, "mending close-ended: --actor is required".to_string()))?;
    let scope = flag(args, "--batch");
    let rows = conn.query(&format!("SELECT {COLUMNS} FROM mending WHERE state IN ('WAITING', 'MENDING')")).map_err(cannot)?;
    let rows = rows.iter().map(from_json).collect::<Result<Vec<_>, _>>().map_err(cannot)?;
    let ended: BTreeSet<String> = conn.query(BATCH_ENDED).map_err(cannot)?.iter().filter_map(|r| cell(r, "batch_id")).collect();
    let steps = plan_close_ended(&rows, &ended, scope.as_deref(), &actor, crate::db::now_epoch());
    let mut closed = 0;
    for step in &steps {
        let applied = matches!(persist(conn, step), Ok(true));
        closed += usize::from(applied && matches!(step.event.kind, MendingEventKind::Expire { .. } | MendingEventKind::Return { .. }));
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
pub(crate) struct Move {
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

pub(crate) struct Prior {
    pub tip: Option<String>,
    pub diagnosis: Option<String>,
}

pub(crate) struct MendInput {
    pub members: Vec<Move>,
    pub facts: MendFacts,
    pub fences: Vec<String>,
    pub actor: String,
    pub prior: BTreeMap<String, Vec<Prior>>,
    pub ask_exists: bool,
}

/// `err` is the refusal the caller reports; with none, `steps` is the one Mended step to record.
pub(crate) struct MendPlan {
    pub steps: Vec<Step>,
    pub err: Option<(i32, String)>,
}

fn refuse_logged(row: &MendingRow, refusal: Refusal, actor: &str, now: i64) -> MendPlan {
    let err = (REFUSED, format!("refused: {refusal:?}"));
    MendPlan { steps: vec![plan_refused(row, refusal, actor, now)], err: Some(err) }
}

/// A mend the machine will not take returns the failure's row with the reason as its diagnosis.
fn return_with(row: &MendingRow, why: &str, refusal: Refusal, actor: &str, now: i64) -> MendPlan {
    let diagnosis = match &row.diagnosis {
        Some(d) => format!("{d}\n{why}"),
        None => why.to_string(),
    };
    let step = plan_step(row, MendingEventKind::Return { at: now, diagnosis }, actor, now);
    let err = match &step.refusal {
        Some(_) => (REFUSED, format!("refused: {:?}", step.refusal)),
        None => (REFUSED, format!("refused: {refusal:?}; the failure is returned")),
    };
    MendPlan { steps: vec![step], err: Some(err) }
}

fn may_mend(row: &MendingRow, actor: &str) -> bool {
    row.state == MendingState::Mending || (row.state == MendingState::Waiting && mending::may_settle_unpicked(actor))
}

pub(crate) fn plan_mend(row: &MendingRow, input: &MendInput, now: i64) -> MendPlan {
    let actor = input.actor.as_str();
    if !may_mend(row, actor) {
        return refuse_logged(row, Refusal::IllegalTransition { state: row.state.as_str().into(), event: "Mended".into() }, actor, now);
    }
    for m in &input.members {
        let prior = input.prior.get(&m.bead).map_or(&[][..], Vec::as_slice);
        if prior.len() >= MEND_CAP_PER_BEAD {
            let diagnoses: Vec<&str> = prior.iter().filter_map(|p| p.diagnosis.as_deref()).collect();
            let why = format!("mend cap: {} was already mended {} times; diagnoses: {}", m.bead, prior.len(), diagnoses.join(" | "));
            return return_with(row, &why, Refusal::MendCap { bead: m.bead.clone(), mends: prior.len() }, actor, now);
        }
        if let Some(current) = prior.last().and_then(|p| p.tip.clone()) {
            if current != m.from {
                let why = format!("fix no longer applies: {} moved to {current} under this mend, which was built on {}", m.bead, m.from);
                return return_with(row, &why, Refusal::StaleTip { bead: m.bead.clone(), current, given: m.from.clone() }, actor, now);
            }
        }
    }
    if let Some(ask) = input.facts.ask.as_deref().filter(|a| !a.is_empty()) {
        if !input.ask_exists {
            return refuse_logged(row, Refusal::MendRefused { reasons: vec![format!("ask {ask} does not exist")] }, actor, now);
        }
    }
    if let Err(refusal) = mending::assess(&input.facts, &input.fences) {
        return refuse_logged(row, refusal, actor, now);
    }
    let mut step = plan_step(row, MendingEventKind::Mended { at: now }, actor, now);
    match &step.refusal {
        None => {
            let members: Vec<String> = input.members.iter().map(|m| format!("{}:{}:{}", m.bead, m.from, m.to)).collect();
            *step.record.evidence_mut() = serde_json::json!({"Mended": {"at": now, "members": members, "ask": input.facts.ask, "checked": input.facts.checked}});
            MendPlan { steps: vec![step], err: None }
        }
        Some(refusal) => {
            let refusal = refusal.clone();
            refuse_logged(row, refusal, actor, now)
        }
    }
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
    let mut prior = BTreeMap::new();
    for m in members.iter().filter(|_| may_mend(&row, &actor)) {
        let mends = mends_of(conn, &m.bead).map_err(cannot)?;
        prior.insert(m.bead.clone(), mends.iter().map(|p| Prior { tip: cell(p, "tip"), diagnosis: cell(p, "diagnosis") }).collect());
    }
    let ask_exists = match facts.ask.as_deref().filter(|a| !a.is_empty()).filter(|_| may_mend(&row, &actor)) {
        Some(ask) => crate::rows::fetch_ask(conn, ask).map_err(cannot)?.is_some(),
        None => false,
    };
    let at = crate::db::now_epoch();
    let input = MendInput { members, facts, fences, actor, prior, ask_exists };
    let plan = plan_mend(&row, &input, at);
    let Some(step) = plan.steps.first().filter(|_| plan.err.is_none()) else {
        for step in &plan.steps {
            if !persist(conn, step).map_err(cannot)? && step.refusal.is_none() {
                return Err((REFUSED, "refused: lost the race to another writer".into()));
            }
        }
        return Err(plan.err.unwrap_or((CANNOT_TELL, "mending mend: no plan".into())));
    };
    let MendInput { members, facts, .. } = input;
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
    let cascade = CascadeStep {
        table: "mending",
        key_column: MENDING_KEY,
        key: row.key(),
        old_version: row.version,
        set_clause: set_clause(&step.after),
        applied_to_state: step.after.state.as_str().to_string(),
        event: step.record.clone(),
    };
    match conn.cascade(&inserts, &[cascade]).map_err(cannot)?.first().copied().unwrap_or(false) {
        true => Ok(step.after.state.as_str().to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_over_long_field_is_refused_by_name_before_sql() {
        assert!(check_widths(&"b".repeat(BATCH_MAX), 1, &"s".repeat(SUITE_MAX)).is_ok());
        let (_, msg) = check_widths(&"b".repeat(BATCH_MAX + 1), 1, "s").unwrap_err();
        assert!(msg.contains("batch_id is 65 chars (max 64)"), "{msg}");
        let (_, msg) = check_widths("b", 1, &"s".repeat(SUITE_MAX + 1)).unwrap_err();
        assert!(msg.contains("suite is 161 chars (max 160)"), "{msg}");
        let schema = include_str!("../../lifecycle/schema.sql");
        for (table, column, max) in [("mending", "batch_id", BATCH_MAX), ("mending", "suite", SUITE_MAX), ("mend", "batch_id", BATCH_MAX), ("mend", "suite", SUITE_MAX), ("event", "lc_key", KEY_MAX)] {
            let body = schema.split(&format!("CREATE TABLE IF NOT EXISTS {table} (")).nth(1).and_then(|t| t.split(");").next()).unwrap_or_default();
            let line = body.lines().find(|l| l.split_whitespace().next() == Some(column)).unwrap_or_default();
            assert!(line.contains(&format!("VARCHAR({max})")), "{table}.{column} in schema.sql is not {max} wide: {line:?}");
        }
        assert!(KEY_MAX >= BATCH_MAX + SUITE_MAX + 2 + u32::MAX.to_string().len(), "the widest triple fits the key column");
    }

    use lifecycle::batch::{self, BatchEvent, BatchEventKind, BatchPhase, BatchRow, BatchState};

    const NOW: i64 = 1_000_000;
    const TIP_A: &str = "a99c65e15e286c45e353db428bf9c3a0b4d89b41";
    const TIP_B: &str = "2728b0b551d31353098466c9b738654ac52f5bda";
    const TIP_C: &str = "a785b91ebd8c184bae68153eee6a61c963debef0";

    fn after(steps: Vec<Step>) -> MendingRow {
        let [step] = <[Step; 1]>::try_from(steps).unwrap_or_else(|s| panic!("want one step, got {}", s.len()));
        assert!(step.refusal.is_none(), "refused: {:?}", step.refusal);
        step.after
    }

    fn waiting(suite: &str) -> MendingRow {
        MendingRow::failed("batch-mend", 1, suite)
    }

    fn picked(suite: &str, deadline_s: u64, at: i64) -> MendingRow {
        after(plan_pickup(&waiting(suite), deadline_s, "mender", at).unwrap())
    }

    fn batch_step(row: &BatchRow, kind: BatchEventKind) -> lifecycle::Outcome<BatchRow> {
        batch::apply(row, &BatchEvent { expect: row.state, version: row.version, kind, actor: "test".into() })
    }

    fn suites_phase() -> BatchRow {
        let row = BatchRow::cut("batch-mend", "r", "headM", "baseM");
        let row = batch_step(&row, BatchEventKind::PassStarted { n: 1, head: "headM".into() }).row;
        let row = batch_step(&row, BatchEventKind::SuitesStarted { n: 1 }).row;
        assert_eq!((row.state, row.phase), (BatchState::CiRunning, Some(BatchPhase::Suites)));
        row
    }

    fn failed(suite: &str) -> BatchEventKind {
        BatchEventKind::Failed { suite: suite.into(), n: 1 }
    }

    #[test]
    fn a_failure_reported_before_the_suites_phase_is_refused_naming_the_phase() {
        let row = batch_step(&BatchRow::cut("batch-mend", "r", "headM", "baseM"), BatchEventKind::PassStarted { n: 1, head: "headM".into() }).row;
        let out = batch_step(&row, failed("test-early.sh"));
        assert!(!out.applied);
        let Some(Refusal::IllegalTransition { state, .. }) = out.refusal else { panic!("{:?}", out.refusal) };
        assert!(state.contains("phase build"), "{state}");
    }

    fn plan(row: &BatchRow, suite: &str, recorded: Option<&MendingRow>) -> FailedPlan {
        plan_failed(row, ("CI_RUNNING", row.state, row.version), "test", &failed(suite), recorded, NOW)
    }

    fn planned(p: FailedPlan) -> (lifecycle::Outcome<BatchRow>, EventRecord, MendingRow) {
        match p {
            FailedPlan::Planned { outcome, record, opens } => (outcome, record, opens),
            FailedPlan::Recorded(m) => panic!("recorded: {m}"),
        }
    }

    #[test]
    fn red_suites_are_recorded_in_order_and_neither_stop_nor_judge_the_pass() {
        let row = suites_phase();
        let (first, rec1, _) = planned(plan(&row, "test-z.sh", None));
        let (second, rec2, _) = planned(plan(&first.row, "test-b.sh", None));
        assert!(first.applied && second.applied);
        assert_eq!((first.row.version, second.row.version), (row.version + 1, row.version + 2));
        let suites: Vec<String> = [&rec1, &rec2].iter().map(|r| r.evidence()["Failed"]["suite"].as_str().unwrap().to_string()).collect();
        assert_eq!(suites, ["test-z.sh", "test-b.sh"]);
        assert_eq!((second.row.state, second.row.phase), (BatchState::CiRunning, Some(BatchPhase::Suites)));
    }

    #[test]
    fn a_suite_reported_twice_is_already_recorded_and_appends_no_event() {
        let row = suites_phase();
        let (_, _, opened) = planned(plan(&row, "test-b.sh", None));
        let FailedPlan::Recorded(msg) = plan(&row, "test-b.sh", Some(&opened)) else { panic!("planned a second event") };
        assert_eq!(msg, "already recorded: test-b.sh failed in pass 1");
    }

    #[test]
    fn a_failure_reported_before_the_suites_phase_is_refused_by_the_plan_naming_the_phase() {
        let row = batch_step(&BatchRow::cut("batch-mend", "r", "headM", "baseM"), BatchEventKind::PassStarted { n: 1, head: "headM".into() }).row;
        let (out, rec, _) = planned(plan(&row, "test-early.sh", None));
        assert!(!out.applied);
        assert_eq!(rec.event_name(), "Failed");
        let Some(Refusal::IllegalTransition { state, .. }) = out.refusal else { panic!("{:?}", out.refusal) };
        assert!(state.contains("phase build"), "{state}");
    }


    #[test]
    fn each_failure_opens_a_waiting_row_with_no_deadline() {
        let row = suites_phase();
        for suite in ["test-z.sh", "test-b.sh"] {
            let (out, _, r) = planned(plan(&row, suite, None));
            assert!(out.applied);
            assert_eq!((r.state, r.deadline, r.picked_at, r.version), (MendingState::Waiting, None, None, 0));
            assert_eq!(r.key(), format!("batch-mend#1#{suite}"));
        }
    }

    #[test]
    fn a_pickup_sets_the_deadline_to_now_plus_the_seconds_given() {
        assert_eq!(picked("test-z.sh", 8, NOW).deadline, Some(NOW + 8));
        let b = picked("test-b.sh", 600, NOW);
        assert_eq!((b.state, b.picked_at, b.deadline), (MendingState::Mending, Some(NOW), Some(NOW + 600)));
    }

    #[test]
    fn a_mender_cannot_expire_a_row_itself() {
        let row = picked("test-b.sh", 600, NOW);
        let (rc, msg) = plan_event(&row, Some(r#"{"Expire":{"at":0}}"#), "mender-b", NOW).unwrap_err();
        assert_eq!(rc, REFUSED);
        assert!(msg.contains("the machine's own"), "{msg}");
    }

    #[test]
    fn a_mender_cannot_move_its_deadline_by_picking_up_again() {
        let row = picked("test-b.sh", 600, NOW);
        let (rc, _) = plan_event(&row, Some(r#"{"Pickup":{"at":0,"deadline_s":99999}}"#), "mender-b", NOW + 5).unwrap_err();
        assert_eq!(rc, REFUSED);
        assert_eq!(row.deadline, Some(NOW + 600));
    }

    #[test]
    fn a_bare_mended_event_cannot_bypass_the_assessment() {
        let row = picked("test-b.sh", 600, NOW);
        let (rc, msg) = plan_event(&row, Some(r#"{"Mended":{"at":0}}"#), "mender", NOW).unwrap_err();
        assert_eq!(rc, REFUSED);
        assert!(msg.contains("mending mend"), "{msg}");
    }

    #[test]
    fn a_diagnosis_event_is_stamped_without_moving_the_deadline() {
        let row = picked("test-z.sh", 8, NOW);
        let r = after(plan_event(&row, Some(r#"{"Diagnosis":{"text":"half a diagnosis"}}"#), "mender-z", NOW + 1).unwrap());
        assert_eq!((r.diagnosis.as_deref(), r.deadline), (Some("half a diagnosis"), Some(NOW + 8)));
    }

    fn two_rows() -> Vec<MendingRow> {
        let z = after(plan_event(&picked("test-z.sh", 8, NOW), Some(r#"{"Diagnosis":{"text":"half a diagnosis"}}"#), "mender-z", NOW).unwrap());
        vec![z, picked("test-b.sh", 600, NOW)]
    }

    #[test]
    fn a_sweep_one_second_before_the_deadline_expires_nothing() {
        assert!(plan_expire_overdue(&two_rows(), "test", NOW + 7).is_empty());
    }

    #[test]
    fn a_sweep_at_the_deadline_returns_exactly_the_overdue_row_with_its_diagnosis() {
        let rows = two_rows();
        let steps = plan_expire_overdue(&rows, "test", NOW + 8);
        let [step] = <[Step; 1]>::try_from(steps).unwrap_or_else(|s| panic!("want one step, got {}", s.len()));
        assert!(step.refusal.is_none());
        assert_eq!(step.before.suite, "test-z.sh");
        assert_eq!((step.after.state, step.after.diagnosis.as_deref()), (MendingState::Returned, Some("half a diagnosis")));
        assert_eq!(rows[1].state, MendingState::Mending, "the other row is not touched");
    }

    #[test]
    fn a_late_outcome_is_refused_once_the_machine_has_returned_the_row() {
        let returned = plan_expire_overdue(&two_rows(), "test", NOW + 8).remove(0).after;
        for kind in [MendingEventKind::Mended { at: NOW + 9 }, MendingEventKind::Unsure { at: NOW + 9 }] {
            let step = plan_step(&returned, kind, "mender-z", NOW + 9);
            assert!(matches!(step.refusal, Some(Refusal::Terminal { .. })), "{:?}", step.refusal);
            assert_eq!(step.after, returned);
        }
    }

    #[test]
    fn the_expiry_is_logged_on_the_mending_row_under_the_sweeps_actor() {
        let step = plan_expire_overdue(&two_rows(), "sweeper", NOW + 8).remove(0);
        assert_eq!((step.record.machine(), step.record.event_name(), step.record.actor()), ("mending", "Expire", "sweeper"));
        assert_eq!(step.event.actor, "sweeper");
    }

    fn input(from: &str, to: &str, path: &str, prior: Vec<Prior>) -> MendInput {
        let facts = MendFacts {
            changes: vec![lifecycle::mending::FileChange { path: path.into(), added: 1, deleted: 0 }],
            member_files: vec![path.into()],
            ..MendFacts::default()
        };
        MendInput {
            members: vec![Move { bead: "sp-lc-m-1".into(), from: from.into(), to: to.into() }],
            facts,
            fences: vec!["gate/".into()],
            actor: "mender-b".into(),
            prior: BTreeMap::from([("sp-lc-m-1".to_string(), prior)]),
            ask_exists: false,
        }
    }

    fn took(tip: &str) -> Prior {
        Prior { tip: Some(tip.into()), diagnosis: Some("earlier".into()) }
    }

    #[test]
    fn a_fence_edit_without_an_ask_is_refused_and_leaves_the_row_mending() {
        let row = picked("test-b.sh", 600, NOW);
        let plan = plan_mend(&row, &input(TIP_A, TIP_B, "gate/x.rs", vec![]), NOW + 1);
        assert_eq!(plan.err.as_ref().map(|e| e.0), Some(REFUSED));
        let step = &plan.steps[0];
        assert!(matches!(step.refusal, Some(Refusal::MendRefused { .. })), "{:?}", step.refusal);
        assert_eq!(step.after.state, MendingState::Mending);
    }

    #[test]
    fn an_in_diff_source_edit_is_mended_and_records_the_branch_tip() {
        let row = picked("test-b.sh", 600, NOW);
        let plan = plan_mend(&row, &input(TIP_A, TIP_B, "queue/src/a.rs", vec![]), NOW + 1);
        assert!(plan.err.is_none(), "{:?}", plan.err);
        let step = &plan.steps[0];
        assert_eq!(step.after.state, MendingState::Mended);
        let members = step.record.evidence()["Mended"]["members"].to_string();
        assert!(members.contains(&format!("sp-lc-m-1:{TIP_A}:{TIP_B}")), "{members}");
    }

    #[test]
    fn a_mend_built_on_a_stale_tip_is_refused_and_the_failure_returned() {
        let row = picked("test-c.sh", 600, NOW);
        let plan = plan_mend(&row, &input(TIP_A, TIP_C, "queue/src/a.rs", vec![took(TIP_B)]), NOW + 1);
        let (rc, msg) = plan.err.clone().unwrap();
        assert_eq!(rc, REFUSED);
        assert!(msg.contains("StaleTip"), "{msg}");
        let step = &plan.steps[0];
        assert_eq!(step.after.state, MendingState::Returned);
        assert!(step.after.diagnosis.as_deref().unwrap().contains("fix no longer applies"));
    }

    #[test]
    fn the_second_mend_of_a_bead_is_taken() {
        let row = picked("test-d.sh", 600, NOW);
        let plan = plan_mend(&row, &input(TIP_B, TIP_C, "queue/src/a.rs", vec![took(TIP_B)]), NOW + 1);
        assert!(plan.err.is_none(), "{:?}", plan.err);
        assert_eq!(plan.steps[0].after.state, MendingState::Mended);
    }

    #[test]
    fn the_third_mend_of_a_bead_is_refused_at_the_cap_and_the_failure_returned() {
        let row = picked("test-e.sh", 600, NOW);
        let plan = plan_mend(&row, &input(TIP_C, TIP_A, "queue/src/a.rs", vec![took(TIP_B), took(TIP_C)]), NOW + 1);
        let (rc, msg) = plan.err.clone().unwrap();
        assert_eq!(rc, REFUSED);
        assert!(msg.contains("MendCap"), "{msg}");
        let step = &plan.steps[0];
        assert_eq!(step.after.state, MendingState::Returned);
        assert!(step.after.diagnosis.as_deref().unwrap().contains("mend cap"));
    }

    #[test]
    fn a_mend_of_an_unpicked_row_is_refused_unless_the_actor_may_settle_it() {
        let mut inp = input(TIP_A, TIP_B, "queue/src/a.rs", vec![]);
        assert_eq!(plan_mend(&waiting("test-b.sh"), &inp, NOW).err.map(|e| e.0), Some(REFUSED));
        inp.actor = "concierge".into();
        assert!(plan_mend(&waiting("test-b.sh"), &inp, NOW).err.is_none());
    }

    #[test]
    fn a_named_ask_that_does_not_exist_refuses_the_mend() {
        let mut inp = input(TIP_A, TIP_B, "gate/x.rs", vec![]);
        inp.facts.ask = Some("ask-1".into());
        let plan = plan_mend(&picked("test-b.sh", 600, NOW), &inp, NOW + 1);
        assert!(plan.err.unwrap().1.contains("does not exist"));
        inp.ask_exists = true;
        assert!(plan_mend(&picked("test-b.sh", 600, NOW), &inp, NOW + 1).err.is_none());
    }

    fn open_row(batch: &str, suite: &str) -> MendingRow {
        MendingRow::failed(batch, 1, suite)
    }

    fn ended(batches: &[&str]) -> BTreeSet<String> {
        batches.iter().map(|b| b.to_string()).collect()
    }

    #[test]
    fn a_waiting_row_on_an_open_batch_is_untouched() {
        let rows = vec![open_row("batch-ended", "test-e.sh")];
        assert_eq!(rows[0].state, MendingState::Waiting);
        assert!(plan_close_ended(&rows, &ended(&[]), None, "concierge", NOW).is_empty());
    }

    #[test]
    fn closing_a_different_batch_leaves_the_row_alone() {
        let rows = vec![open_row("batch-ended", "test-e.sh")];
        assert!(plan_close_ended(&rows, &ended(&["batch-ended"]), Some("batch-other"), "concierge", NOW).is_empty());
    }

    #[test]
    fn a_closed_batchs_open_row_is_returned_with_its_triage_stamped() {
        let rows = vec![open_row("batch-ended", "test-e.sh")];
        let steps = plan_close_ended(&rows, &ended(&["batch-ended"]), None, "concierge", NOW);
        let kinds: Vec<&str> = steps.iter().map(|s| s.record.event_name()).collect();
        assert_eq!(kinds, ["TriageEnded", "Return"]);
        assert!(steps.iter().all(|s| s.refusal.is_none()));
        let last = &steps[1].after;
        assert_eq!((last.state, last.triage_ended_at), (MendingState::Returned, Some(NOW)));
        assert!(last.diagnosis.as_deref().unwrap().contains("the batch closed"));
    }

    #[test]
    fn a_live_batchs_rows_are_untouched_when_another_batch_closes() {
        let rows = vec![open_row("batch-mend", "test-b.sh"), open_row("batch-ended", "test-e.sh")];
        let steps = plan_close_ended(&rows, &ended(&["batch-ended"]), None, "concierge", NOW);
        assert!(!steps.is_empty());
        assert!(steps.iter().all(|s| s.before.batch_id == "batch-ended"));
    }

    #[test]
    fn ending_is_idempotent_because_a_returned_row_is_not_open() {
        let rows = vec![open_row("batch-ended", "test-e.sh")];
        let done = plan_close_ended(&rows, &ended(&["batch-ended"]), None, "concierge", NOW).pop().unwrap().after;
        assert!(plan_close_ended(&[done], &ended(&["batch-ended"]), None, "concierge", NOW + 1).is_empty());
    }

    #[test]
    fn an_overdue_mending_row_of_a_closed_batch_expires_instead_of_returning() {
        let rows = vec![picked("test-b.sh", 10, NOW)];
        let steps = plan_close_ended(&rows, &ended(&["batch-mend"]), None, "concierge", NOW + 10);
        assert_eq!(steps.iter().map(|s| s.record.event_name()).collect::<Vec<_>>(), ["Expire"]);
    }

    fn wide_suite() -> String {
        format!("test-lifecycle-cutover-{}.sh", "x".repeat(63))
    }

    fn wide_batch() -> String {
        format!("r-auto-748-1031T110442-{}", "b".repeat(30))
    }

    #[test]
    fn a_failure_of_a_wide_named_suite_opens_its_row() {
        let (batch, suite) = (wide_batch(), wide_suite());
        assert!(check_widths(&batch, 1, &suite).is_ok());
        let row = MendingRow::failed(&batch, 1, &suite);
        assert_eq!((row.state, row.deadline), (MendingState::Waiting, None));
    }

    #[test]
    fn a_wide_keyed_row_is_picked_up_triaged_and_settled() {
        let row = MendingRow::failed(wide_batch(), 1, wide_suite());
        let row = after(plan_pickup(&row, 600, "mender-w", NOW).unwrap());
        assert_eq!(row.state, MendingState::Mending);
        let row = after(plan_event(&row, Some(r#"{"TriageEnded":{"at":0}}"#), "mender-w", NOW + 1).unwrap());
        assert_eq!(row.triage_ended_at, Some(NOW + 1));
        let row = after(plan_event(&row, Some(r#"{"Unsure":{"at":0}}"#), "mender-w", NOW + 2).unwrap());
        assert_eq!(row.state, MendingState::Unsure);
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn an_over_long_suite_is_refused_by_the_verbs_target_naming_the_field() {
        let over = "y".repeat(161);
        let (rc, msg) = parse_target(&argv(&[&wide_batch(), "--pass", "1", "--suite", &over])).unwrap_err();
        assert_eq!(rc, CANNOT_TELL);
        assert!(msg.contains("suite is 161 chars (max 160)"), "{msg}");
    }

    #[test]
    fn a_wide_but_legal_target_is_accepted_by_the_verbs() {
        let suite = wide_suite();
        assert_eq!(parse_target(&argv(&[&wide_batch(), "--pass", "1", "--suite", &suite])).unwrap(), (wide_batch(), 1, suite));
    }

    #[test]
    fn an_over_long_batch_id_is_refused_by_the_event_verbs_target_naming_batch_id() {
        let (rc, msg) = parse_target(&argv(&[&"z".repeat(65), "--pass", "1", "--suite", "s.sh"])).unwrap_err();
        assert_eq!(rc, CANNOT_TELL);
        assert!(msg.contains("batch_id is 65 chars (max 64)"), "{msg}");
    }
}
