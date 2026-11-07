//! The cutover round's own surface (sp-o7nbr): batch creation, and the three cross-machine
//! cascades — land, settle, abandon — that a batch's own transition emits to every member's
//! delivery and bead row in one transaction (design §3.1.3's "the batch machine emits its
//! members' delivery exits"). `show`/`list`/`history`/`event` (main.rs) are unchanged; this
//! module is additive.
//!
//! Row creation is a constructor, not a transition — `BeadRow::filed`/`BatchRow::cut`/
//! `DeliveryRow::start_queue` already say so in `lifecycle` — so `create-bead` and the batch
//! row inserted by `cut` log no event of their own, exactly as replay never needs one to
//! reach the same initial row. The `Deliver`/`MemberAdded`/`Cut` transitions inside `cut`,
//! and every step inside `land`/`settle`/`abandon-batch`, are real events on real rows and
//! are logged like any other transition.

use lifecycle::{batch, bead, delivery};

use crate::db::{CascadeStep, Conn, DbError, EventRecord};
use crate::{refusal_name, rows};

const CANNOT_TELL: i32 = 2;
const REFUSED: i32 = 3;

pub fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

pub fn q(s: &str) -> String {
    format!("'{}'", rows::escape(s))
}

fn cannot_tell(e: DbError) -> (i32, String) {
    (CANNOT_TELL, format!("cannot tell: {e:?}"))
}

/// `id:tip,id:tip` -> `[(id, tip)]`. Empty input is an empty list, not an error — `settle`'s
/// eject/requeue lists are each allowed to be empty.
fn parse_members(s: &str) -> Option<Vec<(String, String)>> {
    if s.is_empty() {
        return Some(Vec::new());
    }
    s.split(',')
        .map(|pair| {
            let (id, tip) = pair.split_once(':')?;
            Some((id.to_string(), tip.to_string()))
        })
        .collect()
}

fn parse_ids(s: &str) -> Vec<String> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.split(',').map(|s| s.to_string()).collect()
    }
}

/// A lifecycle key: `sp-` then a non-empty tail of alphanumerics, `.`, `-` or `_` — prose and bare prefixes are not.
pub fn is_row_key(x: &str) -> bool {
    x.len() > 3
        && x[..3].eq_ignore_ascii_case("sp-")
        && x[3..].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && x.chars().last().is_some_and(|c| c != '-')
}

/// One point-keyed statement: `bead_id` is the primary key, so a duplicate is ignored by the
/// engine rather than probed for by a second read inside the write. With a title or priority
/// the duplicate instead updates just those mirrored columns — a value not given is kept — and
/// never touches `version`, so a mirror write cannot lose a transition's CAS.
fn create_bead_script(id: &str, at: i64, title: Option<&str>, priority: Option<i64>, submitted_tip: Option<&str>, repo: Option<&str>) -> String {
    if title.is_none() && priority.is_none() && submitted_tip.is_none() && repo.is_none() {
        return format!("INSERT IGNORE INTO bead (bead_id, state, holds, version, updated_at, since) VALUES ({}, 'READY', '[]', 0, {at}, {at});\n", q(id));
    }
    let title_sql = title.map_or("NULL".to_string(), |t| q(&t.chars().take(MIRROR_TITLE_CHARS).collect::<String>()));
    let priority_sql = priority.map_or("NULL".to_string(), |p| p.to_string());
    let repo_sql = repo.map_or("NULL".to_string(), q);
    if let Some(tip) = submitted_tip {
        return submitted_create_script(id, at, &title_sql, &priority_sql, &repo_sql, tip);
    }
    format!(
        "INSERT INTO bead (bead_id, state, tip, holds, version, updated_at, since, title, priority, repo) VALUES ({}, 'READY', NULL, '[]', 0, {at}, {at}, {title_sql}, {priority_sql}, {repo_sql}) \
         ON DUPLICATE KEY UPDATE title = COALESCE(VALUES(title), title), priority = COALESCE(VALUES(priority), priority), repo = COALESCE(VALUES(repo), repo);\n",
        q(id)
    )
}

/// One transaction: an untouched READY row is promoted, an absent row is born SUBMITTED, and the
/// Submit event is written once; a row already past that is left alone.
fn submitted_create_script(id: &str, at: i64, title_sql: &str, priority_sql: &str, repo_sql: &str, tip: &str) -> String {
    let (id, tip) = (q(id), q(tip));
    format!(
        "START TRANSACTION;\n\
         UPDATE bead SET state = 'SUBMITTED', tip = {tip}, version = version + 1, updated_at = {at}, since = {at} WHERE bead_id = {id} AND state = 'READY' AND version = 0 AND tip IS NULL;\n\
         INSERT INTO bead (bead_id, state, tip, holds, version, updated_at, since, title, priority, repo) VALUES ({id}, 'SUBMITTED', {tip}, '[]', 1, {at}, {at}, {title_sql}, {priority_sql}, {repo_sql}) \
         ON DUPLICATE KEY UPDATE title = COALESCE(VALUES(title), title), priority = COALESCE(VALUES(priority), priority), repo = COALESCE(VALUES(repo), repo);\n\
         INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, refusal, evidence, actor, at) \
         SELECT 'bead', {id}, 'Submit', 'READY', 'READY', 'SUBMITTED', 1, NULL, JSON_OBJECT('tip', {tip}), 'bead.sh', {at} FROM bead \
         WHERE bead_id = {id} AND state = 'SUBMITTED' AND tip = {tip} AND NOT EXISTS (SELECT 1 FROM event WHERE machine = 'bead' AND lc_key = {id});\n\
         COMMIT;\n"
    )
}

/// `bead.title` is VARCHAR(512).
const MIRROR_TITLE_CHARS: usize = 512;

/// The planning reader `create-bead` asks for a bead's type: `plan items` rows of the ids given.
pub type PlanReader<'a> = dyn FnMut(&[&str]) -> Result<Vec<serde_json::Value>, String> + 'a;

/// `create-bead`'s refusal of a bead whose type has no lifecycle record (an epic, a decision,
/// …), named; `None` lets the create go ahead. The type is the planning plane's, read through
/// `read`; a bead the planning plane cannot answer for is created, so a filing is never stranded
/// rowless by a planning read that failed.
pub fn refuse_no_record_type(args: &[String], read: &mut PlanReader) -> Option<(i32, String)> {
    let id = args.first().filter(|i| is_row_key(i))?;
    let items = read(&[id.as_str()]).ok()?;
    let kind = items.iter().find(|i| i.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str()))?.get("issue_type")?.as_str()?;
    (!spira_config::nonwork::is_work_type(Some(kind))).then(|| {
        (REFUSED, format!("create-bead: {id} is a {kind}, a type with no lifecycle record (its state is the planning plane's) — nothing created"))
    })
}

/// `create-bead <bead-id> [--title T] [--priority 0..4] [--repo NAME] [--submitted-tip SHA]`. With a tip the
/// row is born SUBMITTED at it (an untouched READY row is promoted), so no claimable READY window exists.
pub fn cmd_create_bead(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "create-bead: missing <bead-id>".into());
    };
    if !is_row_key(id) {
        return (CANNOT_TELL, format!("create-bead: {id:?} is not a bead id"));
    }
    let priority = match flag(args, "--priority") {
        None => None,
        Some(p) => match p.parse::<i64>() {
            Ok(n) if (0..=4).contains(&n) => Some(n),
            _ => return (CANNOT_TELL, format!("create-bead: --priority takes 0..4, got {p:?}")),
        },
    };
    let at = crate::db::now_epoch();
    let script = create_bead_script(id, at, flag(args, "--title").as_deref(), priority, flag(args, "--submitted-tip").as_deref(), flag(args, "--repo").as_deref());
    match conn.run_plain_touching(&script, crate::live::Touch::Keys(vec![id.clone()])) {
        Ok(()) => (0, String::new()),
        Err(e) => cannot_tell(e),
    }
}

/// `delete-bead-row <bead-id>` — the store half of `drop-row`, which has already checked the kind.
pub fn cmd_delete_bead_row(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(id) = args.first().filter(|id| is_row_key(id)) else {
        return (CANNOT_TELL, "delete-bead-row: takes <bead-id>".into());
    };
    match conn.run_plain_touching(&format!("DELETE FROM bead WHERE bead_id = {};\n", q(id)), crate::live::Touch::Keys(vec![id.clone()])) {
        Ok(()) => (0, String::new()),
        Err(e) => cannot_tell(e),
    }
}

/// `show-batch <batch-id>` -> `{"batch_id","state","version",...}`, or exit 1 with `{}` when
/// absent. A caller driving `abandon-batch`/`eject-member`/`settle` needs the batch's
/// *current* state and version to build its `--expect`/`--version` pair — those are set by
/// whichever event last applied (verdict.sh's CI outcome, another operator action), not by
/// whatever a caller's own record last saw, so this queries spira_lifecycle fresh rather
/// than letting a caller cache and go stale.
pub fn cmd_show_batch(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(batch_id) = args.first() else {
        return (CANNOT_TELL, "show-batch: missing <batch-id>".into());
    };
    match rows::fetch_batch(conn, batch_id) {
        Ok(Some(row)) => match conn.query(&format!("SELECT progress FROM batch WHERE batch_id = {}", q(batch_id))) {
            Ok(r) => {
                let progress = r.first().and_then(|r| r.get("progress")).and_then(|p| p.as_str()).and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok());
                show_batch_reply(Some((&row, progress)))
            }
            Err(e) => cannot_tell(e),
        },
        Ok(None) => show_batch_reply(None),
        Err(e) => cannot_tell(e),
    }
}

/// A missing batch is exit 1 with `{}`: the miss a caller's guard fails closed on. Built
/// field-by-field, not a derive-serialize of BatchRow: that prints the Rust variant name
/// ("Open"), while every `--expect`/`state` string in this CLI, which a caller feeds back, is
/// `.as_str()`'s upper-snake form ("OPEN").
fn show_batch_reply(found: Option<(&batch::BatchRow, Option<serde_json::Value>)>) -> (i32, String) {
    let Some((row, progress)) = found else { return (1, "{}".to_string()) };
    (
        0,
        serde_json::json!({
            "batch_id": row.batch_id,
            "repo": row.repo,
            "state": row.state.as_str(),
            "parent": row.parent,
            "head": row.head,
            "base": row.base,
            "run": row.run,
            "reason": row.reason,
            "pass": row.pass,
            "phase": row.phase.map(|p| p.as_str()),
            "progress": progress,
            "version": row.version,
        })
        .to_string(),
    )
}

/// The service never sees this process's stdin, so `batch-progress`'s payload is read here and
/// travels as `--json <body>`.
pub fn inline_stdin_payload(args: Vec<String>) -> Vec<String> {
    inline_payload_from(args, &mut std::io::stdin())
}

pub(crate) fn inline_payload_from(mut args: Vec<String>, input: &mut dyn std::io::Read) -> Vec<String> {
    let verb = (args.first().map(String::as_str), args.get(1).map(String::as_str));
    let reads_stdin = matches!(
        verb,
        (Some("batch-progress" | "claim-verdicts"), _)
            | (Some("section"), Some("apply-spec" | "publish-status"))
            | (Some("intent"), Some("put"))
    );
    if reads_stdin && !args.iter().any(|a| a == "--json") {
        let mut body = String::new();
        let _ = input.read_to_string(&mut body);
        args.push("--json".into());
        args.push(body);
    }
    // `rc <verb> --json -`: the body is stdin, carried inline so the service reads it too.
    if args.first().map(String::as_str) == Some(spira_lc::rc::VERB) {
        if let Some(i) = args.iter().position(|a| a == "--json").map(|i| i + 1).filter(|&i| args.get(i).is_some_and(|b| b == "-")) {
            let mut body = String::new();
            let _ = input.read_to_string(&mut body);
            args[i] = body;
        }
    }
    let ids_on_stdin = args.first().is_some_and(|v| v == "plan") && args.windows(2).any(|w| w[0] == "--ids" && w[1] == "-");
    if ids_on_stdin {
        let mut body = String::new();
        let _ = input.read_to_string(&mut body);
        let ids = body.split(|c: char| c == ',' || c.is_whitespace()).filter(|x| !x.is_empty()).collect::<Vec<_>>().join(",");
        if let Some(i) = args.windows(2).position(|w| w[0] == "--ids" && w[1] == "-") {
            args[i + 1] = ids;
        }
    }
    args
}

/// `batch-progress <batch-id>`, a JSON object on stdin: records the running pass's live counts on
/// the batch row. It is a reading of the pass, not a transition — no event, no version bump.
pub fn cmd_batch_progress(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(batch_id) = args.first() else {
        return (CANNOT_TELL, "batch-progress: missing <batch-id>".into());
    };
    let mut body = String::new();
    if let Some(inline) = flag(args, "--json") {
        body = inline;
    } else if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut body) {
        return (CANNOT_TELL, format!("batch-progress: stdin: {e}"));
    }
    if !serde_json::from_str::<serde_json::Value>(&body).is_ok_and(|v| v.is_object()) {
        return (CANNOT_TELL, "batch-progress: stdin must be one JSON object".into());
    }
    match conn.run_plain(&format!("UPDATE batch SET progress = {} WHERE batch_id = {}", q(body.trim()), q(batch_id))) {
        Ok(()) => (0, String::new()),
        Err(e) => cannot_tell(e),
    }
}

/// What a verb will write, built from rows already read and never touching the store.
pub(crate) struct Plan {
    pub preamble: String,
    pub steps: Vec<CascadeStep>,
    /// `"applied": null` marks where the run's result is written, so the key order is the plan's.
    pub summary: serde_json::Value,
}

/// A verb's refusal. `record` is the refused event the store keeps; it is absent when the verb
/// refused before any event was tried.
pub(crate) struct Refused {
    pub code: i32,
    pub text: String,
    pub record: Option<Box<EventRecord>>,
}

impl From<(i32, String)> for Refused {
    fn from((code, text): (i32, String)) -> Self {
        Refused { code, text, record: None }
    }
}

type Planned = Result<Plan, Refused>;

fn run_plan(conn: &Conn, mut plan: Plan) -> (i32, String) {
    match conn.cascade(&plan.preamble, &plan.steps) {
        Ok(applied) => {
            let all_ok = applied.iter().all(|a| *a);
            if plan.summary.get("applied").is_some() {
                plan.summary["applied"] = serde_json::json!(applied);
            }
            (if all_ok { 0 } else { REFUSED }, plan.summary.to_string())
        }
        Err(e) => cannot_tell(e),
    }
}

fn refuse(conn: &Conn, r: Refused) -> (i32, String) {
    match r.record {
        Some(rec) => match conn.insert_refusal_event(&rec) {
            Ok(()) => (r.code, r.text),
            Err(e) => cannot_tell(e),
        },
        None => (r.code, r.text),
    }
}


/// The bead and delivery rows of the beads a verb touches, by id; an id with no row is absent.
#[derive(Default)]
pub(crate) struct Roster {
    pub beads: std::collections::BTreeMap<String, bead::BeadRow>,
    pub deliveries: std::collections::BTreeMap<String, delivery::DeliveryRow>,
}

/// Everything `land`, `settle`, `abandon-batch`, `eject-member`, `stack` and `promote` read.
/// `contained` is the git fact `branch_containment` computes and `returned` the members whose
/// last `Returned` came after their last `Deliver`.
pub(crate) struct Rows {
    pub batch: batch::BatchRow,
    pub members: Vec<(String, String)>,
    pub roster: Roster,
    pub contained: std::collections::BTreeSet<(String, String)>,
    pub returned: std::collections::BTreeSet<String>,
}

fn fetch_roster<'a>(conn: &Conn, ids: impl IntoIterator<Item = &'a String>) -> Result<Roster, DbError> {
    let mut roster = Roster::default();
    for id in ids {
        if let Some(row) = rows::fetch_bead(conn, id)? {
            roster.beads.insert(id.clone(), row);
        }
        if let Some(row) = rows::fetch_delivery(conn, id)? {
            roster.deliveries.insert(id.clone(), row);
        }
    }
    Ok(roster)
}

/// The batch's own members and every member and `extra` id's rows.
fn fetch_rows(conn: &Conn, batch: batch::BatchRow, extra: &[String]) -> Result<Rows, DbError> {
    let members = fetch_members(conn, &batch.batch_id)?;
    let roster = fetch_roster(conn, members.iter().map(|(id, _)| id).chain(extra))?;
    Ok(Rows { batch, members, roster, contained: Default::default(), returned: Default::default() })
}

fn member_ids(members: &[(String, String)]) -> Vec<&String> {
    members.iter().map(|(i, _)| i).collect()
}

fn insert_member_sql(batch_id: &str, id: &str, tip: &str) -> String {
    format!("INSERT INTO batch_member (batch_id, bead_id, tip) VALUES ({}, {}, {});\n", q(batch_id), q(id), q(tip))
}

pub(crate) struct StageArgs {
    pub batch_id: String,
    pub repo: String,
    pub head: String,
    pub base: String,
    pub members: Vec<(String, String)>,
    pub parent: String,
}

/// `stage <batch-id> --repo R --head H --base B --members "id:tip,..." --actor A --parent P`
///
/// A round assembled behind the still-running round `P`: the batch row is written STAGED with
/// its `batch_member` rows and nothing else. No member is delivered, so a staged round that is
/// discarded returns nothing; `promote` is what cuts it.
pub fn cmd_stage(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(batch_id) = args.first() else {
        return (CANNOT_TELL, "stage: missing <batch-id>".into());
    };
    let (Some(repo), Some(head), Some(base), Some(members_s), Some(_actor), Some(parent)) =
        (flag(args, "--repo"), flag(args, "--head"), flag(args, "--base"), flag(args, "--members"), flag(args, "--actor"), flag(args, "--parent"))
    else {
        return (CANNOT_TELL, "stage: --repo, --head, --base, --members, --actor and --parent are all required".into());
    };
    let Some(members) = parse_members(&members_s) else {
        return (CANNOT_TELL, "stage: --members must be \"id:tip,id:tip,...\"".into());
    };
    if members.is_empty() {
        return (CANNOT_TELL, "stage: --members must name at least one bead".into());
    }
    let roster = match fetch_roster(conn, members.iter().map(|(id, _)| id)) {
        Ok(r) => r,
        Err(e) => return cannot_tell(e),
    };
    let stage = StageArgs { batch_id: batch_id.clone(), repo, head, base, members, parent };
    match plan_stage(&roster, &stage, crate::db::now_epoch()) {
        Ok(plan) => run_plan(conn, plan),
        Err(r) => refuse(conn, r),
    }
}

pub(crate) fn plan_stage(roster: &Roster, args: &StageArgs, at: i64) -> Planned {
    admit_members(&args.members, roster, true)?;
    let mut preamble = format!(
        "INSERT INTO batch (batch_id, repo, state, parent, head, base, run, version, opened_at)\n\
         VALUES ({}, {}, 'STAGED', {}, {}, {}, NULL, 0, {at});\n",
        q(&args.batch_id),
        q(&args.repo),
        q(&args.parent),
        q(&args.head),
        q(&args.base),
    );
    for (id, tip) in &args.members {
        preamble.push_str(&insert_member_sql(&args.batch_id, id, tip));
    }
    let summary = serde_json::json!({"batch_id": args.batch_id, "members": member_ids(&args.members)});
    Ok(Plan { preamble, steps: Vec::new(), summary })
}

pub(crate) struct PromoteArgs {
    pub batch_id: String,
    pub head: String,
    pub base: String,
    pub actor: String,
}

/// `promote <batch-id> --head H --base B --actor A`
///
/// STAGED -> OPEN at the head and base the members were re-merged onto, and in the same
/// transaction the cascade `cut` performs for every staged member. Every member must still be
/// SUBMITTED or CERTIFIED at its staged tip; one that moved refuses the whole promotion,
/// before anything is written.
pub fn cmd_promote(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(batch_id) = args.first() else {
        return (CANNOT_TELL, "promote: missing <batch-id>".into());
    };
    let (Some(head), Some(base), Some(actor)) = (flag(args, "--head"), flag(args, "--base"), flag(args, "--actor")) else {
        return (CANNOT_TELL, "promote: --head, --base and --actor are all required".into());
    };
    let batch_row = match rows::fetch_batch(conn, batch_id) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("promote: no batch row for {batch_id}")),
        Err(e) => return cannot_tell(e),
    };
    let rows = match fetch_rows(conn, batch_row, &[]) {
        Ok(r) => r,
        Err(e) => return cannot_tell(e),
    };
    let promote = PromoteArgs { batch_id: batch_id.clone(), head, base, actor };
    match plan_promote(&rows, &promote, crate::db::now_epoch()) {
        Ok(plan) => run_plan(conn, plan),
        Err(r) => refuse(conn, r),
    }
}

pub(crate) fn plan_promote(rows: &Rows, args: &PromoteArgs, at: i64) -> Planned {
    let batch_row = &rows.batch;
    let batch_id = &args.batch_id;
    if batch_row.state != batch::BatchState::Staged {
        return Err((REFUSED, format!("refused: {batch_id} is {} not STAGED", batch_row.state.as_str())).into());
    }
    let bead_rows = admit_members(&rows.members, &rows.roster, true)?;
    let delivery_rows = admit_deliveries(&rows.members, &rows.roster)?;
    let promote_ev = batch::BatchEvent {
        expect: batch::BatchState::Staged,
        version: batch_row.version,
        kind: batch::BatchEventKind::Promote { head: args.head.clone(), base: args.base.clone() },
        actor: args.actor.clone(),
    };
    let outcome = batch::apply(batch_row, &promote_ev);
    if !outcome.applied {
        return Err((REFUSED, format!("refused: {:?}", outcome.refusal)).into());
    }
    let mut steps = vec![CascadeStep {
        table: "batch",
        key_column: "batch_id",
        key: batch_id.clone(),
        old_version: batch_row.version,
        set_clause: rows::batch_set_clause(&outcome.row),
        applied_to_state: outcome.row.state.as_str().to_string(),
        event: EventRecord::of_apply("batch", batch_id, "Promote", "STAGED", batch_row.state.as_str(), None, serde_json::to_value(&promote_ev.kind).unwrap_or_default(), &args.actor, at),
    }];
    let mut preamble = String::new();
    steps.extend(member_added_steps(batch_id, outcome.row, &rows.members, &bead_rows, &delivery_rows, &args.actor, at, &mut preamble, "promote")?);
    let summary = serde_json::json!({"batch_id": batch_id, "members": member_ids(&rows.members), "applied": null});
    Ok(Plan { preamble, steps, summary })
}

pub(crate) struct CutArgs {
    pub batch_id: String,
    pub repo: String,
    pub head: String,
    pub base: String,
    pub members: Vec<(String, String)>,
    pub parent: Option<String>,
    pub actor: String,
}

/// `cut <batch-id> --repo R --head H --base B --members "id:tip,id:tip" --actor A [--parent P]`
///
/// Every named member must currently be CERTIFIED (or SUBMITTED) at exactly the given tip — checked before
/// anything is written, so a stale caller refuses cleanly instead of cutting a batch with
/// the wrong content. On success, in one transaction: the batch row is created (OPEN), one
/// `batch_member` row and one `MemberAdded` batch event per member, the bead's `Deliver`
/// event, and the delivery row's `Cut` event (fresh -> BATCHED).
pub fn cmd_cut(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(batch_id) = args.first() else {
        return (CANNOT_TELL, "cut: missing <batch-id>".into());
    };
    let (Some(repo), Some(head), Some(base), Some(members_s), Some(actor)) =
        (flag(args, "--repo"), flag(args, "--head"), flag(args, "--base"), flag(args, "--members"), flag(args, "--actor"))
    else {
        return (CANNOT_TELL, "cut: --repo, --head, --base, --members and --actor are all required".into());
    };
    let parent = flag(args, "--parent");
    let Some(members) = parse_members(&members_s) else {
        return (CANNOT_TELL, "cut: --members must be \"id:tip,id:tip,...\"".into());
    };
    if members.is_empty() {
        return (CANNOT_TELL, "cut: --members must name at least one bead".into());
    }
    let roster = match fetch_roster(conn, members.iter().map(|(id, _)| id)) {
        Ok(r) => r,
        Err(e) => return cannot_tell(e),
    };
    let cut = CutArgs { batch_id: batch_id.clone(), repo, head, base, members, parent, actor };
    match plan_cut(&roster, &cut, crate::db::now_epoch()) {
        Ok(plan) => run_plan(conn, plan),
        Err(r) => refuse(conn, r),
    }
}

pub(crate) fn plan_cut(roster: &Roster, args: &CutArgs, at: i64) -> Planned {
    let bead_rows = admit_members(&args.members, roster, true)?;
    let delivery_rows = admit_deliveries(&args.members, roster)?;
    let mut preamble = format!(
        "INSERT INTO batch (batch_id, repo, state, parent, head, base, run, version, opened_at)\n\
         VALUES ({}, {}, 'OPEN', {}, {}, {}, NULL, 0, {at});\n",
        q(&args.batch_id),
        q(&args.repo),
        args.parent.as_deref().map(q).unwrap_or_else(|| "NULL".into()),
        q(&args.head),
        q(&args.base),
    );
    for (id, tip) in &args.members {
        preamble.push_str(&insert_member_sql(&args.batch_id, id, tip));
    }
    let mut batch_row = batch::BatchRow::cut(args.batch_id.as_str(), args.repo.as_str(), args.head.as_str(), args.base.as_str());
    // `cut()` has no way to know this is a bisect child; every step's SET clause is built from
    // this row, so the parent must be on it or the first step writes the constructor's NULL back.
    batch_row.parent = args.parent.clone();
    let steps = member_added_steps(&args.batch_id, batch_row, &args.members, &bead_rows, &delivery_rows, &args.actor, at, &mut preamble, "cut")?;
    let summary = serde_json::json!({"batch_id": args.batch_id, "members": member_ids(&args.members), "applied": null});
    Ok(Plan { preamble, steps, summary })
}

pub(crate) struct StackArgs {
    pub batch_id: String,
    pub members: Vec<(String, String)>,
    pub actor: String,
}

/// `stack <batch-id> --members "id:tip,id:tip" --actor A`
///
/// batcher-cut's own pipelining onto an already-OPEN batch (law-queue-back-pressure-is-
/// an-open-pr): the same `MemberAdded`/`Deliver`/`Cut` cascade `cut` performs for a fresh
/// batch, minus the batch row's own INSERT — it already exists and must stay OPEN, which
/// is checked before anything is written, same as `cut` checks every member's own
/// CERTIFIED tip.
pub fn cmd_stack(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(batch_id) = args.first() else {
        return (CANNOT_TELL, "stack: missing <batch-id>".into());
    };
    let (Some(members_s), Some(actor)) = (flag(args, "--members"), flag(args, "--actor")) else {
        return (CANNOT_TELL, "stack: --members and --actor are both required".into());
    };
    let Some(members) = parse_members(&members_s) else {
        return (CANNOT_TELL, "stack: --members must be \"id:tip,id:tip,...\"".into());
    };
    if members.is_empty() {
        return (CANNOT_TELL, "stack: --members must name at least one bead".into());
    }
    let batch_row = match rows::fetch_batch(conn, batch_id) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("stack: no batch row for {batch_id}")),
        Err(e) => return cannot_tell(e),
    };
    let roster = match fetch_roster(conn, members.iter().map(|(id, _)| id)) {
        Ok(r) => r,
        Err(e) => return cannot_tell(e),
    };
    let rows = Rows { batch: batch_row, members: Vec::new(), roster, contained: Default::default(), returned: Default::default() };
    let stack = StackArgs { batch_id: batch_id.clone(), members, actor };
    match plan_stack(&rows, &stack, crate::db::now_epoch()) {
        Ok(plan) => run_plan(conn, plan),
        Err(r) => refuse(conn, r),
    }
}

pub(crate) fn plan_stack(rows: &Rows, args: &StackArgs, at: i64) -> Planned {
    let batch_id = &args.batch_id;
    if rows.batch.state != batch::BatchState::Open {
        return Err((REFUSED, format!("refused: {batch_id} is {} not OPEN", rows.batch.state.as_str())).into());
    }
    let bead_rows = admit_members(&args.members, &rows.roster, false)?;
    let delivery_rows = admit_deliveries(&args.members, &rows.roster)?;
    let mut preamble = String::new();
    for (id, tip) in &args.members {
        preamble.push_str(&insert_member_sql(batch_id, id, tip));
    }
    let steps = member_added_steps(batch_id, rows.batch.clone(), &args.members, &bead_rows, &delivery_rows, &args.actor, at, &mut preamble, "stack")?;
    let summary = serde_json::json!({"batch_id": batch_id, "members": member_ids(&args.members), "applied": null});
    Ok(Plan { preamble, steps, summary })
}

/// Every named member must currently be CERTIFIED at exactly the given tip — checked
/// before anything is written, so a stale caller (`cut` or `stack` alike) refuses
/// cleanly instead of admitting a member with the wrong content.
/// `cut` passes `allow_submitted`: a round takes SUBMITTED beads directly, its own full suite
/// being the certification (law-a-round-is-feature-first-then-catch-all); `stack` does not.
fn admit_members(members: &[(String, String)], roster: &Roster, allow_submitted: bool) -> Result<Vec<bead::BeadRow>, (i32, String)> {
    let mut bead_rows = Vec::new();
    for (id, tip) in members {
        let Some(row) = roster.beads.get(id) else {
            return Err((REFUSED, format!("refused: no bead row for {id}")));
        };
        let admitted = row.state == bead::BeadState::Certified || (allow_submitted && row.state == bead::BeadState::Submitted);
        if !admitted {
            let want = if allow_submitted { "SUBMITTED or CERTIFIED" } else { "CERTIFIED" };
            return Err((REFUSED, format!("refused: {id} is {} not {want}", row.state.as_str())));
        }
        if row.tip.as_deref() != Some(tip.as_str()) {
            return Err((REFUSED, format!("refused: {id}'s certified tip does not match {tip}")));
        }
        bead_rows.push(row.clone());
    }
    Ok(bead_rows)
}

/// Each member's existing delivery row, or None. A row still BATCHED in another batch is
/// that batch's member: the cut refuses, naming the batch. Only a QUEUED or EXITED row can
/// be cut.
fn admit_deliveries(members: &[(String, String)], roster: &Roster) -> Result<Vec<Option<delivery::DeliveryRow>>, (i32, String)> {
    let mut out = Vec::new();
    for (id, _) in members {
        let row = roster.deliveries.get(id);
        if let Some(r) = row {
            match r.state {
                delivery::DeliveryState::Queued | delivery::DeliveryState::Exited => {}
                delivery::DeliveryState::Batched => {
                    return Err((REFUSED, format!("refused: {id} is still BATCHED in {}", r.batch_id.as_deref().unwrap_or("another batch"))));
                }
                s => return Err((REFUSED, format!("refused: {id}'s delivery is {} and cannot be cut", s.as_str()))),
            }
        }
        out.push(row.cloned());
    }
    Ok(out)
}
/// The `MemberAdded` (batch) + `Deliver` (bead) + `Enqueued`/`Cut` (delivery) cascade steps for each of `members`, folded
/// onto `batch_row` in order so each step's CAS'd version follows the last. Shared by
/// `cut` (a freshly constructed OPEN row) and `stack` (an existing one fetched from the
/// database) — both hand this the same batch row shape and get the same steps back.
#[allow(clippy::too_many_arguments)]
fn member_added_steps(
    batch_id: &str,
    mut batch_row: batch::BatchRow,
    members: &[(String, String)],
    bead_rows: &[bead::BeadRow],
    delivery_rows: &[Option<delivery::DeliveryRow>],
    actor: &str,
    at: i64,
    preamble: &mut String,
    verb: &str,
) -> Result<Vec<CascadeStep>, (i32, String)> {
    let mut steps = Vec::new();
    for (((id, tip), bead_row), delivery_row) in members.iter().zip(bead_rows.iter()).zip(delivery_rows.iter()) {
        let member_ev = batch::BatchEvent {
            expect: batch_row.state,
            version: batch_row.version,
            kind: batch::BatchEventKind::MemberAdded { bead_id: id.clone(), tip: tip.clone() },
            actor: actor.to_string(),
        };
        let outcome = batch::apply(&batch_row, &member_ev);
        if !outcome.applied {
            return Err((CANNOT_TELL, format!("{verb}: MemberAdded refused unexpectedly for {id}: {:?}", outcome.refusal)));
        }
        steps.push(CascadeStep {
            table: "batch",
            key_column: "batch_id",
            key: batch_id.to_string(),
            old_version: batch_row.version,
            set_clause: rows::batch_set_clause(&outcome.row),
            applied_to_state: outcome.row.state.as_str().to_string(),
            event: EventRecord::of_apply("batch", batch_id, "MemberAdded", batch_row.state.as_str(), batch_row.state.as_str(), None, serde_json::to_value(&member_ev.kind).unwrap_or_default(), actor, at),
        });
        batch_row = outcome.row;

        let deliver_ev = bead::BeadEvent {
            expect: bead_row.state,
            version: bead_row.version,
            kind: bead::BeadEventKind::Deliver,
            actor: actor.to_string(),
            at: Some(at),
        };
        let bead_outcome = bead::apply(bead_row, &deliver_ev);
        if !bead_outcome.applied {
            return Err((CANNOT_TELL, format!("{verb}: Deliver refused unexpectedly for {id}: {:?}", bead_outcome.refusal)));
        }
        steps.push(CascadeStep {
            table: "bead",
            key_column: "bead_id",
            key: id.clone(),
            old_version: bead_row.version,
            set_clause: rows::bead_set_clause(&bead_outcome.row),
            applied_to_state: bead_outcome.row.state.as_str().to_string(),
            event: EventRecord::of_apply("bead", id, "Deliver", bead_row.state.as_str(), bead_row.state.as_str(), None, serde_json::to_value(&deliver_ev.kind).unwrap_or_default(), actor, at),
        });

        let (queued, requeue_step) = match delivery_row {
            None => {
                let fresh = delivery::DeliveryRow::start_queue(id.as_str());
                let enqueued = EventRecord::import_absent("delivery", id, "Enqueued", serde_json::Value::Null, actor, at);
                preamble.push_str(&format!(
                    "INSERT INTO delivery (bead_id, mode, state, batch_id, pr, merge_sha, version)\n\
                     VALUES ({id}, 'queue', 'QUEUED', NULL, NULL, NULL, {version});\n",
                    id = q(id),
                    version = fresh.version,
                ));
                preamble.push_str(&enqueued.applied_insert_sql(delivery::DeliveryState::Queued.as_str()));
                (fresh, None)
            }
            Some(row) if row.state == delivery::DeliveryState::Exited => {
                let mut fresh = delivery::DeliveryRow::start_queue(id.as_str());
                fresh.version = row.version + 1;
                let step = CascadeStep {
                    table: "delivery",
                    key_column: "bead_id",
                    key: id.clone(),
                    old_version: row.version,
                    set_clause: format!("mode = 'queue', state = 'QUEUED', batch_id = NULL, pr = NULL, merge_sha = NULL, version = {}", fresh.version),
                    applied_to_state: delivery::DeliveryState::Queued.as_str().to_string(),
                    event: EventRecord::of_apply("delivery", id, "Enqueued", row.state.as_str(), row.state.as_str(), None, serde_json::Value::Null, actor, at),
                };
                (fresh, Some(step))
            }
            Some(row) => (row.clone(), None),
        };
        steps.extend(requeue_step);
        let cut_ev = delivery::DeliveryEvent {
            expect: delivery::DeliveryState::Queued,
            version: queued.version,
            kind: delivery::DeliveryEventKind::Cut { batch_id: batch_id.to_string() },
            actor: actor.to_string(),
        };
        let delivery_outcome = delivery::apply(&queued, &cut_ev);
        if !delivery_outcome.applied {
            return Err((CANNOT_TELL, format!("{verb}: Cut refused unexpectedly for {id}: {:?}", delivery_outcome.refusal)));
        }
        steps.push(CascadeStep {
            table: "delivery",
            key_column: "bead_id",
            key: id.clone(),
            old_version: queued.version,
            set_clause: rows::delivery_set_clause(&delivery_outcome.row),
            applied_to_state: delivery_outcome.row.state.as_str().to_string(),
            event: EventRecord::of_apply("delivery", id, "Cut", queued.state.as_str(), queued.state.as_str(), None, serde_json::to_value(&cut_ev.kind).unwrap_or_default(), actor, at),
        });
    }
    Ok(steps)
}

pub(crate) struct LandArgs {
    pub batch_id: String,
    pub expect: batch::BatchState,
    pub version: u64,
    pub actor: String,
    pub sha: String,
}

/// `land <batch-id> --expect S --version V --actor A --sha SHA`
///
/// One transaction: `FastForward{sha}` on the batch (S -> LANDED), and for every member,
/// `Delivered{merge_sha,proof}` on its delivery row (-> EXITED) and its bead row (-> LANDED).
pub fn cmd_land(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(batch_id) = args.first() else {
        return (CANNOT_TELL, "land: missing <batch-id>".into());
    };
    let (Some(expect), Some(version_s), Some(actor), Some(sha)) =
        (flag(args, "--expect"), flag(args, "--version"), flag(args, "--actor"), flag(args, "--sha"))
    else {
        return (CANNOT_TELL, "land: --expect, --version, --actor and --sha are all required".into());
    };
    let Ok(version) = version_s.parse::<u64>() else {
        return (CANNOT_TELL, "land: --version must be a non-negative integer".into());
    };
    let Some(expect_state) = batch::BatchState::from_str(&expect) else {
        return (CANNOT_TELL, format!("land: unknown batch state {expect:?}"));
    };

    let batch_row = match rows::fetch_batch(conn, batch_id) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("land: no batch row for {batch_id}")),
        Err(e) => return cannot_tell(e),
    };
    let rows = match fetch_rows(conn, batch_row, &[]) {
        Ok(r) => r,
        Err(e) => return cannot_tell(e),
    };
    let land = LandArgs { batch_id: batch_id.clone(), expect: expect_state, version, actor, sha };
    let plan = match plan_land(&rows, &land, crate::db::now_epoch()) {
        Ok(p) => p,
        Err(r) => return refuse(conn, r),
    };
    let out = run_plan(conn, plan);
    if out.0 == 0 {
        crate::cycle::push_landed(conn, &rows.members.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>());
    }
    out
}

pub(crate) fn plan_land(rows: &Rows, args: &LandArgs, at: i64) -> Planned {
    let (batch_id, actor, sha) = (&args.batch_id, &args.actor, &args.sha);
    let expect = args.expect.as_str();
    let ff_ev = batch::BatchEvent {
        expect: args.expect,
        version: args.version,
        kind: batch::BatchEventKind::FastForward { sha: sha.clone() },
        actor: actor.clone(),
    };
    let outcome = batch::apply(&rows.batch, &ff_ev);
    let evidence = serde_json::to_value(&ff_ev.kind).unwrap_or_default();
    if !outcome.applied {
        let record = EventRecord::of_apply("batch", batch_id, "FastForward", expect, rows.batch.state.as_str(), outcome.refusal.as_ref().map(refusal_name), evidence, actor, at);
        return Err(Refused { code: REFUSED, text: format!("refused: {:?}", outcome.refusal), record: Some(Box::new(record)) });
    }

    let mut steps = vec![CascadeStep {
        table: "batch",
        key_column: "batch_id",
        key: batch_id.clone(),
        old_version: args.version,
        set_clause: rows::batch_set_clause(&outcome.row),
        applied_to_state: outcome.row.state.as_str().to_string(),
        event: EventRecord::of_apply("batch", batch_id, "FastForward", expect, rows.batch.state.as_str(), None, evidence, actor, at),
    }];

    for (id, tip) in &rows.members {
        let Some(delivery_row) = rows.roster.deliveries.get(id) else { continue };
        if !still_in_batch(delivery_row, batch_id) {
            continue;
        }
        let delivered_ev = delivery::DeliveryEvent {
            expect: delivery_row.state,
            version: delivery_row.version,
            kind: delivery::DeliveryEventKind::Delivered { merge_sha: sha.clone(), proof: format!("queue-fast-forward:{tip}") },
            actor: actor.clone(),
        };
        let d_outcome = delivery::apply(delivery_row, &delivered_ev);
        if d_outcome.applied {
            steps.push(cascade_step(
                "delivery",
                "bead_id",
                id,
                delivery_row.version,
                rows::delivery_set_clause(&d_outcome.row),
                d_outcome.row.state.as_str(),
                "delivery",
                "Delivered",
                delivery_row.state.as_str(),
                &delivered_ev.kind,
                actor,
                at,
            ));
        }

        let Some(bead_row) = rows.roster.beads.get(id) else { continue };
        let bead_delivered_ev = bead::BeadEvent {
            expect: bead_row.state,
            version: bead_row.version,
            kind: bead::BeadEventKind::Delivered { merge_sha: sha.clone(), proof: format!("queue-fast-forward:{tip}") },
            actor: actor.clone(),
            at: Some(at),
        };
        let b_outcome = bead::apply(bead_row, &bead_delivered_ev);
        if b_outcome.applied {
            steps.push(cascade_step(
                "bead",
                "bead_id",
                id,
                bead_row.version,
                rows::bead_set_clause(&b_outcome.row),
                b_outcome.row.state.as_str(),
                "bead",
                "Delivered",
                bead_row.state.as_str(),
                &bead_delivered_ev.kind,
                actor,
                at,
            ));
        }
    }

    let summary = serde_json::json!({"batch_id": batch_id, "steps": steps.len(), "applied": null});
    Ok(Plan { preamble: String::new(), steps, summary })
}

pub(crate) struct SettleArgs {
    pub batch_id: String,
    pub expect: batch::BatchState,
    pub version: u64,
    pub actor: String,
    pub eject: Vec<String>,
    pub requeue: Vec<String>,
}

/// `settle <batch-id> --expect S --version V --actor A [--eject "id,id"] [--requeue "id,id"]`
///
/// One transaction: `Settle` on the batch (S -> SETTLED); ejected members get `Returned` on
/// delivery and bead (-> REWORK); requeued members get `Requeued` (-> CERTIFIED, or SUBMITTED
/// if the bead's tip no longer matches — the tip invariant, design §3.1).
pub fn cmd_settle(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(batch_id) = args.first() else {
        return (CANNOT_TELL, "settle: missing <batch-id>".into());
    };
    let (Some(expect), Some(version_s), Some(actor)) = (flag(args, "--expect"), flag(args, "--version"), flag(args, "--actor")) else {
        return (CANNOT_TELL, "settle: --expect, --version and --actor are all required".into());
    };
    let Ok(version) = version_s.parse::<u64>() else {
        return (CANNOT_TELL, "settle: --version must be a non-negative integer".into());
    };
    let Some(expect_state) = batch::BatchState::from_str(&expect) else {
        return (CANNOT_TELL, format!("settle: unknown batch state {expect:?}"));
    };
    let eject = parse_ids(&flag(args, "--eject").unwrap_or_default());
    let requeue = parse_ids(&flag(args, "--requeue").unwrap_or_default());

    let batch_row = match rows::fetch_batch(conn, batch_id) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("settle: no batch row for {batch_id}")),
        Err(e) => return cannot_tell(e),
    };
    let mut rows = match fetch_rows(conn, batch_row, &eject) {
        Ok(r) => r,
        Err(e) => return cannot_tell(e),
    };
    rows.contained = branch_containment(&rows.batch, &rows.members);
    let settle = SettleArgs { batch_id: batch_id.clone(), expect: expect_state, version, actor, eject, requeue };
    match plan_settle(&rows, &settle, crate::db::now_epoch()) {
        Ok(plan) => run_plan(conn, plan),
        Err(r) => refuse(conn, r),
    }
}

pub(crate) fn plan_settle(rows: &Rows, args: &SettleArgs, at: i64) -> Planned {
    let (batch_id, actor) = (&args.batch_id, &args.actor);
    let expect = args.expect.as_str();
    let settle_ev = batch::BatchEvent { expect: args.expect, version: args.version, kind: batch::BatchEventKind::Settle, actor: actor.clone() };
    let outcome = batch::apply(&rows.batch, &settle_ev);
    if !outcome.applied {
        let record = EventRecord::of_apply("batch", batch_id, "Settle", expect, rows.batch.state.as_str(), outcome.refusal.as_ref().map(refusal_name), serde_json::Value::String("Settle".into()), actor, at);
        return Err(Refused { code: REFUSED, text: format!("refused: {:?}", outcome.refusal), record: Some(Box::new(record)) });
    }

    let mut steps = vec![CascadeStep {
        table: "batch",
        key_column: "batch_id",
        key: batch_id.clone(),
        old_version: args.version,
        set_clause: rows::batch_set_clause(&outcome.row),
        applied_to_state: outcome.row.state.as_str().to_string(),
        event: EventRecord::of_apply("batch", batch_id, "Settle", expect, rows.batch.state.as_str(), None, serde_json::Value::String("Settle".into()), actor, at),
    }];

    // Closure (design stacked-dependents-2026-09-28 §3): ejecting a member ejects every
    // member of this same round stacked on it, transitively, whichever member attribution blamed.
    let cascade: Vec<(String, String)> =
        stacked_dependents_in_batch(rows, &args.eject).into_iter().filter(|(id, _)| !args.eject.contains(id)).collect();
    let cascade_ids: Vec<String> = cascade.iter().map(|(id, _)| id.clone()).collect();
    let requeue_ids: Vec<&String> = args.requeue.iter().filter(|id| !cascade_ids.contains(id) && !args.eject.contains(id)).collect();

    for id in &args.eject {
        exit_steps(&rows.roster, &mut steps, id, delivery::DeliveryEventKind::Returned { reason: lifecycle::reason::ReturnedReason::BatchEjectedRed }, actor, at);
    }
    // A stacked dependent is collateral, not itself red: `base-withdrawn`, not
    // `batch-ejected`, so the reconciler's rework-by-cause query can tell them apart.
    for id in &cascade_ids {
        exit_steps(&rows.roster, &mut steps, id, delivery::DeliveryEventKind::Returned { reason: lifecycle::reason::ReturnedReason::BaseWithdrawn }, actor, at);
    }
    name_prerequisites(&mut steps, &cascade);
    for id in requeue_ids {
        let Some((_, tip)) = rows.members.iter().find(|(mid, _)| mid == id) else { continue };
        exit_steps(&rows.roster, &mut steps, id, delivery::DeliveryEventKind::Requeued { tip: tip.clone() }, actor, at);
    }

    // The meter (design §3): how many members this settle carried down as collateral, and how
    // deep each ejected or cascaded member's own stack was.
    let mut stack_depth = serde_json::Map::new();
    for id in args.eject.iter().chain(cascade_ids.iter()) {
        if let Some(row) = rows.roster.beads.get(id) {
            stack_depth.insert(id.clone(), serde_json::json!(row.stack_depth));
        }
    }
    let summary = serde_json::json!({
        "batch_id": batch_id,
        "steps": steps.len(),
        "applied": null,
        "base_withdrawn": cascade_ids.len(),
        "stacked_on": cascade.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
        "stack_depth": stack_depth,
    });
    Ok(Plan { preamble: String::new(), steps, summary })
}

/// True when the bead's last `Returned` came after its last `Deliver`: the queue ejected it
/// from this delivery, whatever the bead and delivery rows say now.
fn returned_since_delivery(conn: &Conn, id: &str) -> Result<bool, DbError> {
    let key = rows::escape(id);
    let sql = format!(
        "SELECT 1 AS hit FROM event WHERE machine = 'bead' AND lc_key = '{key}' AND applied = 1 AND event = 'Returned' \
         AND seq > COALESCE((SELECT MAX(seq) FROM event WHERE machine = 'bead' AND lc_key = '{key}' AND applied = 1 AND event = 'Deliver'), 0) LIMIT 1"
    );
    Ok(!conn.query(&sql)?.is_empty())
}

/// The exit a member takes when its round is released: an ejected bead keeps its verdict
/// (`Returned`), any other returns to where it entered delivery (`Requeued`).
fn release_kind_of(returned: bool, tip: String) -> delivery::DeliveryEventKind {
    if returned {
        delivery::DeliveryEventKind::Returned { reason: lifecycle::reason::ReturnedReason::BatchEjectedRed }
    } else {
        delivery::DeliveryEventKind::Requeued { tip }
    }
}

fn release_kind(conn: &Conn, id: &str, tip: String) -> Result<delivery::DeliveryEventKind, DbError> {
    Ok(release_kind_of(returned_since_delivery(conn, id)?, tip))
}

pub(crate) struct AbandonArgs {
    pub batch_id: String,
    pub expect: batch::BatchState,
    pub version: u64,
    pub actor: String,
    pub reason: String,
}

/// `abandon-batch <batch-id> --expect S --version V --actor A --reason R`
///
/// One transaction: `Abandon{reason}` on the batch (any non-terminal S -> ABANDONED), and
/// for every member's delivery and bead row the exit `release_kind` picks: `Requeued` to the
/// state it entered delivery from, or `Returned` for a member the queue had ejected.
pub fn cmd_abandon_batch(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(batch_id) = args.first() else {
        return (CANNOT_TELL, "abandon-batch: missing <batch-id>".into());
    };
    let (Some(expect), Some(version_s), Some(actor), Some(reason)) =
        (flag(args, "--expect"), flag(args, "--version"), flag(args, "--actor"), flag(args, "--reason"))
    else {
        return (CANNOT_TELL, "abandon-batch: --expect, --version, --actor and --reason are all required".into());
    };
    let Ok(version) = version_s.parse::<u64>() else {
        return (CANNOT_TELL, "abandon-batch: --version must be a non-negative integer".into());
    };
    let Some(expect_state) = batch::BatchState::from_str(&expect) else {
        return (CANNOT_TELL, format!("abandon-batch: unknown batch state {expect:?}"));
    };

    let batch_row = match rows::fetch_batch(conn, batch_id) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("abandon-batch: no batch row for {batch_id}")),
        Err(e) => return cannot_tell(e),
    };
    let mut rows = match fetch_rows(conn, batch_row, &[]) {
        Ok(r) => r,
        Err(e) => return cannot_tell(e),
    };
    for (id, _) in &rows.members {
        match returned_since_delivery(conn, id) {
            Ok(true) => {
                rows.returned.insert(id.clone());
            }
            Ok(false) => {}
            Err(e) => return cannot_tell(e),
        }
    }
    let abandon = AbandonArgs { batch_id: batch_id.clone(), expect: expect_state, version, actor, reason };
    match plan_abandon(&rows, &abandon, crate::db::now_epoch()) {
        Ok(plan) => run_plan(conn, plan),
        Err(r) => refuse(conn, r),
    }
}

pub(crate) fn plan_abandon(rows: &Rows, args: &AbandonArgs, at: i64) -> Planned {
    let (batch_id, actor) = (&args.batch_id, &args.actor);
    let expect = args.expect.as_str();
    let ev = batch::BatchEvent { expect: args.expect, version: args.version, kind: batch::BatchEventKind::Abandon { reason: args.reason.clone() }, actor: actor.clone() };
    let outcome = batch::apply(&rows.batch, &ev);
    let evidence = serde_json::to_value(&ev.kind).unwrap_or_default();
    if !outcome.applied {
        let record = EventRecord::of_apply("batch", batch_id, "Abandon", expect, rows.batch.state.as_str(), outcome.refusal.as_ref().map(refusal_name), evidence, actor, at);
        return Err(Refused { code: REFUSED, text: format!("refused: {:?}", outcome.refusal), record: Some(Box::new(record)) });
    }

    let mut steps = vec![CascadeStep {
        table: "batch",
        key_column: "batch_id",
        key: batch_id.clone(),
        old_version: args.version,
        set_clause: rows::batch_set_clause(&outcome.row),
        applied_to_state: outcome.row.state.as_str().to_string(),
        event: EventRecord::of_apply("batch", batch_id, "Abandon", expect, rows.batch.state.as_str(), None, evidence, actor, at),
    }];
    for (id, tip) in &rows.members {
        exit_steps(&rows.roster, &mut steps, id, release_kind_of(rows.returned.contains(id), tip.clone()), actor, at);
    }
    let summary = serde_json::json!({"batch_id": batch_id, "steps": steps.len(), "applied": null});
    Ok(Plan { preamble: String::new(), steps, summary })
}

/// A round that opened and then showed no sign of life: no PR, no run, no pass, no phase.
const PHANTOM_BATCH_SECS: i64 = 1800;

/// `requeue-orphans --actor A [--apply] [--stale-secs N]`
///
/// A bead IN_DELIVERY that no open batch names is held by nothing and invisible to every
/// round. So is the member of a batch that opened and never began: OPEN, no PR, run, pass or
/// phase, older than `--stale-secs`. Lists them; with `--apply` an orphan goes back through
/// `Requeued{tip}` in its own transaction (so a SUBMITTED member returns to SUBMITTED, a
/// CERTIFIED one to CERTIFIED); a member the queue ejected since it entered delivery is
/// `Returned` instead, so it ends REWORK and only its stale delivery row is cleared and a phantom batch is abandoned with all its members.
/// Beads whose delivery is live outside a batch (pr, push, local) are not orphans.
pub fn cmd_requeue_orphans(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(actor) = flag(args, "--actor") else {
        return (CANNOT_TELL, "requeue-orphans: --actor is required".into());
    };
    let apply = args.iter().any(|a| a == "--apply");
    let stale_secs = match flag(args, "--stale-secs") {
        None => PHANTOM_BATCH_SECS,
        Some(s) => match s.parse::<i64>() {
            Ok(n) if n >= 0 => n,
            _ => return (CANNOT_TELL, "requeue-orphans: --stale-secs must be a non-negative integer".into()),
        },
    };
    let sql = "SELECT bead_id FROM bead WHERE state = 'IN_DELIVERY' AND bead_id NOT IN \
               (SELECT bm.bead_id FROM batch_member bm JOIN batch b ON b.batch_id = bm.batch_id \
                WHERE b.state NOT IN ('LANDED', 'SETTLED', 'ABANDONED')) ORDER BY bead_id";
    let found = match conn.query(sql) {
        Ok(r) => r,
        Err(e) => return cannot_tell(e),
    };
    let mut orphans = Vec::new();
    for r in &found {
        let Some(id) = r.get("bead_id").and_then(|v| v.as_str()) else { continue };
        match rows::fetch_delivery(conn, id) {
            Ok(Some(d)) if d.mode != delivery::Mode::Queue && !d.state.is_terminal() => continue,
            Ok(_) => orphans.push(id.to_string()),
            Err(e) => return cannot_tell(e),
        }
    }

    let phantom_sql = format!(
        "SELECT batch_id, version FROM batch WHERE state = 'OPEN' AND pr IS NULL AND run IS NULL AND pass = 0 AND phase IS NULL \
         AND opened_at < {} ORDER BY batch_id",
        crate::db::now_epoch() - stale_secs
    );
    let phantoms = match conn.query(&phantom_sql) {
        Ok(r) => r,
        Err(e) => return cannot_tell(e),
    };
    let mut phantom_batches: Vec<(String, String)> = Vec::new();
    for r in &phantoms {
        let (Some(id), Some(v)) = (r.get("batch_id").and_then(|v| v.as_str()), r.get("version").and_then(|v| v.as_str())) else { continue };
        phantom_batches.push((id.to_string(), v.to_string()));
    }
    let mut phantom_members: Vec<String> = Vec::new();
    for (batch_id, _) in &phantom_batches {
        match fetch_members(conn, batch_id) {
            Ok(m) => phantom_members.extend(m.into_iter().map(|(id, _)| id)),
            Err(e) => return cannot_tell(e),
        }
    }
    orphans.extend(phantom_members.iter().filter(|id| !orphans.contains(id)).cloned().collect::<Vec<_>>());
    orphans.sort();

    let mut requeued = Vec::new();
    let mut failed = Vec::new();
    let mut abandoned = Vec::new();
    if apply {
        for (batch_id, version) in &phantom_batches {
            let why = format!("requeue-orphans: opened and never began (no PR, run, pass or phase in {stale_secs}s)");
            let abandon_args: Vec<String> =
                [batch_id.as_str(), "--expect", "OPEN", "--version", version, "--actor", &actor, "--reason", &why].iter().map(|s| s.to_string()).collect();
            match cmd_abandon_batch(&abandon_args, conn) {
                (0, _) => abandoned.push(batch_id.clone()),
                _ => failed.push(batch_id.clone()),
            }
        }
        for id in &orphans {
            if phantom_members.contains(id) {
                if !phantom_batches.iter().any(|(b, _)| failed.contains(b)) {
                    requeued.push(id.clone());
                }
                continue;
            }
            let at = crate::db::now_epoch();
            let tip = match rows::fetch_bead(conn, id) {
                Ok(Some(b)) => b.tip.unwrap_or_default(),
                Ok(None) => continue,
                Err(e) => return cannot_tell(e),
            };
            let mut steps = Vec::new();
            let kind = match release_kind(conn, id, tip) {
                Ok(k) => k,
                Err(e) => return cannot_tell(e),
            };
            if let Err(e) = add_exit_steps(conn, &mut steps, id, kind, &actor, at) {
                return cannot_tell(e);
            }
            match conn.cascade("", &steps) {
                Ok(applied) if !applied.is_empty() && applied.iter().all(|a| *a) => requeued.push(id.clone()),
                _ => failed.push(id.clone()),
            }
        }
    }
    let summary = serde_json::json!({
        "orphans": orphans,
        "phantom_batches": phantom_batches.iter().map(|(b, _)| b).collect::<Vec<_>>(),
        "abandoned": abandoned,
        "requeued": requeued,
        "failed": failed,
        // Round leases live only in the control store, so on this path none is held.
        "leased": [],
    });
    (if failed.is_empty() { 0 } else { REFUSED }, serde_json::to_string(&summary).unwrap())
}

/// `reconcile-landed <bead-id> --sha SHA --ref REF (--repo-path P | --repo NAME) --actor A`
///
/// Work that verifiably merged while the lifecycle missed it. The proof is ancestry — SHA is
/// reachable from REF — and a message on SHA that names the bead; a tip compare proves
/// neither. One transaction: the bead goes to LANDED from whatever non-terminal state it
/// holds, and a delivery row still BATCHED, PR_OPEN or PUSHING exits Delivered with SHA.
pub fn cmd_reconcile_landed(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(bead_id) = args.first() else {
        return (CANNOT_TELL, "reconcile-landed: missing <bead-id>".into());
    };
    let (Some(sha), Some(base), Some(actor)) = (flag(args, "--sha"), flag(args, "--ref"), flag(args, "--actor")) else {
        return (CANNOT_TELL, "reconcile-landed: --sha, --ref and --actor are required".into());
    };
    let repo_path = match (flag(args, "--repo-path"), flag(args, "--repo")) {
        (Some(p), _) => std::path::PathBuf::from(p),
        (None, Some(name)) => match crate::repo_config::from_process().map(|c| c.repos.get(&name).map(|s| s.path.clone())) {
            Ok(Some(p)) => std::path::PathBuf::from(p),
            Ok(None) => return (CANNOT_TELL, format!("reconcile-landed: no repository {name:?} in the config in force")),
            Err(e) => return (CANNOT_TELL, format!("reconcile-landed: {e}")),
        },
        (None, None) => return (CANNOT_TELL, "reconcile-landed: --repo-path or --repo is required".into()),
    };
    if !crate::git_evidence::is_ancestor(&repo_path, &sha, &base) {
        return (REFUSED, format!("refused: {sha} is not reachable from {base}"));
    }
    if !crate::git_evidence::commit_names_bead(&repo_path, &sha, bead_id) {
        return (REFUSED, format!("refused: the commit message of {sha} does not name {bead_id}"));
    }

    let bead_row = match rows::fetch_bead(conn, bead_id) {
        Ok(Some(r)) => r,
        Ok(None) => return (REFUSED, format!("refused: no bead row for {bead_id}")),
        Err(e) => return cannot_tell(e),
    };
    let at = crate::db::now_epoch();
    let proof = format!("reconcile-landed:{sha}");
    let bead_ev = bead::BeadEvent {
        expect: bead_row.state,
        version: bead_row.version,
        kind: bead::BeadEventKind::ContentOnBase { proof: proof.clone() },
        actor: actor.clone(),
        at: Some(at),
    };
    let bead_outcome = bead::apply(&bead_row, &bead_ev);
    if !bead_outcome.applied {
        return (REFUSED, format!("refused: {:?}", bead_outcome.refusal));
    }

    let mut steps = Vec::new();
    match rows::fetch_delivery(conn, bead_id) {
        Ok(Some(d)) => {
            let ev = delivery::DeliveryEvent {
                expect: d.state,
                version: d.version,
                kind: delivery::DeliveryEventKind::Delivered { merge_sha: sha.clone(), proof },
                actor: actor.clone(),
            };
            let outcome = delivery::apply(&d, &ev);
            if outcome.applied {
                steps.push(cascade_step("delivery", "bead_id", bead_id, d.version, rows::delivery_set_clause(&outcome.row), outcome.row.state.as_str(), "delivery", "Delivered", d.state.as_str(), &ev.kind, &actor, at));
            }
        }
        Ok(None) => {}
        Err(e) => return cannot_tell(e),
    }
    steps.push(cascade_step(
        "bead",
        "bead_id",
        bead_id,
        bead_row.version,
        rows::bead_set_clause(&bead_outcome.row),
        bead_outcome.row.state.as_str(),
        "bead",
        "ContentOnBase",
        bead_row.state.as_str(),
        &bead_ev.kind,
        &actor,
        at,
    ));
    match conn.cascade("", &steps) {
        Ok(applied) => {
            let summary = serde_json::json!({"bead_id": bead_id, "sha": sha, "steps": steps.len(), "applied": applied});
            (if applied.iter().all(|a| *a) { 0 } else { REFUSED }, summary.to_string())
        }
        Err(e) => cannot_tell(e),
    }
}

pub(crate) struct EjectArgs {
    pub batch_id: String,
    pub bead_id: String,
    pub expect: batch::BatchState,
    pub version: u64,
    pub actor: String,
    pub reason: String,
}

/// `eject-member <batch-id> --bead-id ID --expect S --version V --actor A --reason R`
///
/// One transaction: `Eject{bead_id,reason}` on the batch (OPEN, CI_RUNNING or GREEN; only a
/// GREEN batch moves, back to OPEN — survivors stay in the batch), and `Requeued{tip}` for the ejected
/// member's delivery and bead row (tip unchanged, so it resurrects CERTIFIED per the tip
/// invariant, same as `abandon-batch`). This is the manual, pre-CI-outcome eject
/// (queue.sh's `cmd_eject`, an operator/czar action) — distinct from `settle`'s own
/// `--eject` list, which only runs after a real Red event and returns members to REWORK, not
/// CERTIFIED. Synthesizing a Red here to reuse that path would log CI failure that never
/// happened; the event log is the record of what happened.
pub fn cmd_eject_member(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(batch_id) = args.first() else {
        return (CANNOT_TELL, "eject-member: missing <batch-id>".into());
    };
    let (Some(bead_id), Some(expect), Some(version_s), Some(actor), Some(reason)) = (
        flag(args, "--bead-id"),
        flag(args, "--expect"),
        flag(args, "--version"),
        flag(args, "--actor"),
        flag(args, "--reason"),
    ) else {
        return (CANNOT_TELL, "eject-member: --bead-id, --expect, --version, --actor and --reason are all required".into());
    };
    let Ok(version) = version_s.parse::<u64>() else {
        return (CANNOT_TELL, "eject-member: --version must be a non-negative integer".into());
    };
    let Some(expect_state) = batch::BatchState::from_str(&expect) else {
        return (CANNOT_TELL, format!("eject-member: unknown batch state {expect:?}"));
    };

    let batch_row = match rows::fetch_batch(conn, batch_id) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("eject-member: no batch row for {batch_id}")),
        Err(e) => return cannot_tell(e),
    };
    let mut rows = match fetch_rows(conn, batch_row, std::slice::from_ref(&bead_id)) {
        Ok(r) => r,
        Err(e) => return cannot_tell(e),
    };
    rows.contained = branch_containment(&rows.batch, &rows.members);
    let eject = EjectArgs { batch_id: batch_id.clone(), bead_id, expect: expect_state, version, actor, reason };
    match plan_eject_member(&rows, &eject, crate::db::now_epoch()) {
        Ok(plan) => run_plan(conn, plan),
        Err(r) => refuse(conn, r),
    }
}

pub(crate) fn plan_eject_member(rows: &Rows, args: &EjectArgs, at: i64) -> Planned {
    let (batch_id, bead_id, actor) = (&args.batch_id, &args.bead_id, &args.actor);
    let expect = args.expect.as_str();
    let Some(tip) = rows.members.iter().find(|(id, _)| id == bead_id).map(|(_, t)| t.clone()) else {
        return Err((CANNOT_TELL, format!("eject-member: {bead_id} is not a member of {batch_id}")).into());
    };
    let ev = batch::BatchEvent {
        expect: args.expect,
        version: args.version,
        kind: batch::BatchEventKind::Eject { bead_id: bead_id.clone(), reason: args.reason.clone() },
        actor: actor.clone(),
    };
    let outcome = batch::apply(&rows.batch, &ev);
    let evidence = serde_json::to_value(&ev.kind).unwrap_or_default();
    if !outcome.applied {
        let record = EventRecord::of_apply("batch", batch_id, "Eject", expect, rows.batch.state.as_str(), outcome.refusal.as_ref().map(refusal_name), evidence, actor, at);
        return Err(Refused { code: REFUSED, text: format!("refused: {:?}", outcome.refusal), record: Some(Box::new(record)) });
    }

    let mut steps = vec![CascadeStep {
        table: "batch",
        key_column: "batch_id",
        key: batch_id.clone(),
        old_version: args.version,
        set_clause: rows::batch_set_clause(&outcome.row),
        applied_to_state: outcome.row.state.as_str().to_string(),
        event: EventRecord::of_apply("batch", batch_id, "Eject", expect, rows.batch.state.as_str(), None, evidence, actor, at),
    }];
    exit_steps(&rows.roster, &mut steps, bead_id, delivery::DeliveryEventKind::Requeued { tip }, actor, at);

    // A hand eject cascades like a settle's own attribution: every member stacked on `bead_id`,
    // transitively, follows it out as `base-withdrawn`, never `batch-ejected` — this member's own
    // content was never accused of anything.
    let cascade = stacked_dependents_in_batch(rows, std::slice::from_ref(bead_id));
    let cascade_ids: Vec<String> = cascade.iter().map(|(id, _)| id.clone()).collect();
    for id in &cascade_ids {
        exit_steps(&rows.roster, &mut steps, id, delivery::DeliveryEventKind::Returned { reason: lifecycle::reason::ReturnedReason::BaseWithdrawn }, actor, at);
    }
    name_prerequisites(&mut steps, &cascade);

    let summary = serde_json::json!({
        "batch_id": batch_id,
        "bead_id": bead_id,
        "steps": steps.len(),
        "applied": null,
        "base_withdrawn": cascade_ids.len(),
        "stacked_on": cascade.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
    });
    Ok(Plan { preamble: String::new(), steps, summary })
}

/// `batches-of <bead-id>` — the not-yet-finished batches naming the bead as a member, as a JSON
/// array of `{batch_id, state, version}` ready to feed `eject-member`.
pub fn cmd_batches_of(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(bead_id) = args.first() else {
        return (CANNOT_TELL, "batches-of: missing <bead-id>".into());
    };
    let sql = format!(
        "SELECT b.batch_id, b.state, b.version FROM batch_member bm JOIN batch b ON b.batch_id = bm.batch_id \
         WHERE bm.bead_id = {} AND b.state NOT IN ('LANDED', 'SETTLED', 'ABANDONED') ORDER BY b.batch_id",
        q(bead_id)
    );
    match conn.query(&sql) {
        Ok(found) => {
            let text = |r: &serde_json::Value, k: &str| match r.get(k) {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(v) if !v.is_null() => v.to_string(),
                _ => String::new(),
            };
            let out: Vec<serde_json::Value> = found
                .iter()
                .map(|r| serde_json::json!({"batch_id": text(r, "batch_id"), "state": text(r, "state"), "version": text(r, "version")}))
                .collect();
            (0, serde_json::Value::Array(out).to_string())
        }
        Err(e) => cannot_tell(e),
    }
}

/// An ejected member keeps its `batch_member` row but its delivery has left the batch.
fn still_in_batch(row: &delivery::DeliveryRow, batch_id: &str) -> bool {
    row.state == delivery::DeliveryState::Batched && row.batch_id.as_deref() == Some(batch_id)
}

fn fetch_members(conn: &Conn, batch_id: &str) -> Result<Vec<(String, String)>, DbError> {
    let sql = format!("SELECT bead_id, tip FROM batch_member WHERE batch_id = {}", q(batch_id));
    let rows_v = conn.query(&sql)?;
    Ok(rows_v
        .iter()
        .filter_map(|r| {
            let id = r.get("bead_id")?.as_str()?.to_string();
            let tip = r.get("tip")?.as_str()?.to_string();
            Some((id, tip))
        })
        .collect())
}

/// The transitive closure of this batch's own members stacked on any of `roots` (design
/// stacked-dependents-2026-09-28 §3), as `(dependent, prerequisite it was reached through)`.
/// A member `M` is stacked on prerequisite `P` when `M`'s own bead row's `stack` still names
/// `P` at exactly the tip this batch recorded for `P` (re-checked, since a prerequisite can
/// move between assembly and settle), or when `M`'s branch contains a commit of `P`'s own
/// range whether or not anyone declared it. `roots` themselves are never returned.
fn stacked_dependents_in_batch(rows: &Rows, roots: &[String]) -> Vec<(String, String)> {
    let tips: std::collections::BTreeMap<String, String> = rows.members.iter().cloned().collect();
    let member_stacks: Vec<(String, bead::BeadRow)> =
        rows.members.iter().filter_map(|(id, _)| rows.roster.beads.get(id).map(|r| (id.clone(), r.clone()))).collect();
    stacked_dependents_from(&member_stacks, &tips, &rows.contained, roots)
}

/// `(dependent, prerequisite)` for every pair of members where the dependent's branch holds a
/// commit in the prerequisite's own range above the batch's base. A repository, base or tip
/// git cannot resolve yields no edge: the declared `stack` still applies.
pub(crate) fn branch_containment(batch_row: &batch::BatchRow, members: &[(String, String)]) -> std::collections::BTreeSet<(String, String)> {
    let empty = std::collections::BTreeSet::new();
    let Some(base) = batch_row.base.as_deref() else { return empty };
    let Ok(cfg) = crate::repo_config::from_process() else { return empty };
    let Some(section) = cfg.repos.get(&batch_row.repo) else { return empty };
    containment_in_repo(std::path::Path::new(&section.path), base, members)
}

fn containment_in_repo(repo: &std::path::Path, base: &str, members: &[(String, String)]) -> std::collections::BTreeSet<(String, String)> {
    let mut edges = std::collections::BTreeSet::new();
    let ranges: Vec<(&String, std::collections::BTreeSet<String>)> = members
        .iter()
        .filter_map(|(id, tip)| crate::git_evidence::commits_above(repo, base, tip).map(|c| (id, c)))
        .collect();
    for (dependent, theirs) in &ranges {
        for (prereq, ours) in &ranges {
            if dependent != prereq && !ours.is_empty() && ours.len() < theirs.len() && ours.is_subset(theirs) {
                edges.insert(((*dependent).clone(), (*prereq).clone()));
            }
        }
    }
    edges
}

/// The pure frontier walk, split out so the closure is unit-testable without a live `dolt`.
pub(crate) fn stacked_dependents_from(
    member_stacks: &[(String, bead::BeadRow)],
    tips: &std::collections::BTreeMap<String, String>,
    contained: &std::collections::BTreeSet<(String, String)>,
    roots: &[String],
) -> Vec<(String, String)> {
    let mut visited: std::collections::BTreeSet<String> = roots.iter().cloned().collect();
    let mut frontier: Vec<String> = roots.to_vec();
    let mut dependents = Vec::new();
    while let Some(prereq) = frontier.pop() {
        for (id, row) in member_stacks {
            if visited.contains(id) {
                continue;
            }
            let declared = row.stack.get(&prereq).is_some_and(|given_tip| tips.get(&prereq) == Some(given_tip));
            if declared || contained.contains(&(id.clone(), prereq.clone())) {
                visited.insert(id.clone());
                dependents.push((id.clone(), prereq.clone()));
                frontier.push(id.clone());
            }
        }
    }
    dependents
}

/// Writes the prerequisite each cascaded member was stacked on into its `Returned` events'
/// evidence, so the record on the dependent names the member it followed out.
fn name_prerequisites(steps: &mut [CascadeStep], cascade: &[(String, String)]) {
    for step in steps.iter_mut() {
        let Some((_, prereq)) = cascade.iter().find(|(id, _)| id == &step.key) else { continue };
        if step.event.event_name() != "Returned" {
            continue;
        }
        let mut evidence = match std::mem::take(step.event.evidence_mut()) {
            serde_json::Value::Object(m) => m,
            other => serde_json::Map::from_iter([("evidence".to_string(), other)]),
        };
        evidence.insert("stacked_on".into(), serde_json::Value::String(prereq.clone()));
        *step.event.evidence_mut() = serde_json::Value::Object(evidence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn only_a_member_still_batched_in_this_batch_is_delivered() {
        let mut row = delivery::DeliveryRow::start_queue("sp-a");
        assert!(!still_in_batch(&row, "b-1"));
        row.state = delivery::DeliveryState::Batched;
        row.batch_id = Some("b-1".into());
        assert!(still_in_batch(&row, "b-1"));
        assert!(!still_in_batch(&row, "b-2"));
        row.state = delivery::DeliveryState::Exited;
        row.exit = Some(delivery::Exit::Requeued);
        assert!(!still_in_batch(&row, "b-1"));
    }

    #[test]
    fn every_created_row_carries_since() {
        for s in [
            create_bead_script("sp-a", 7, None, None, None, None),
            create_bead_script("sp-a", 7, Some("t"), Some(1), None, Some("spira")),
            create_bead_script("sp-a", 7, Some("t"), Some(2), Some("abc123"), Some("spira")),
        ] {
            for ins in s.split(';').filter(|x| x.contains("INSERT") && x.contains("INTO bead ")) {
                assert!(ins.contains("since"), "{ins}");
            }
        }
    }

    #[test]
    fn a_cascaded_state_change_stamps_since_with_the_events_own_at() {
        let at = 1_000;
        let mut bead_row = bead::BeadRow::filed("sp-a");
        bead_row.state = bead::BeadState::Certified;
        let batch_row = batch::BatchRow::cut("b-1", "r", "h", "base");
        let mut preamble = String::new();
        let steps = member_added_steps("b-1", batch_row, &[("sp-a".into(), "t".into())], &[bead_row], &[None], "x", at, &mut preamble, "v").unwrap();
        let step = steps.iter().find(|s| s.table == "bead").expect("a bead step");
        assert!(step.set_clause.contains("state = 'IN_DELIVERY'"), "{}", step.set_clause);
        assert!(step.set_clause.contains("since = 1000,"), "{}", step.set_clause);
        assert!(step.event.applied_insert_sql("IN_DELIVERY").contains(", 1000);"));
    }

    #[test]
    fn create_bead_is_one_ignoring_insert() {
        let s = create_bead_script("sp-a'b", 7, None, None, None, None);
        assert!(s.starts_with("INSERT IGNORE INTO bead"), "{s}");
        assert!(!s.contains("NOT EXISTS"), "{s}");
        assert_eq!(s.matches(';').count(), 1);
    }

    #[test]
    fn a_mirrored_create_updates_only_the_mirror_columns_and_never_the_version() {
        let s = create_bead_script("sp-a", 7, Some("it's a title"), Some(1), None, None);
        assert!(s.contains("it\\'s a title") && s.contains("ON DUPLICATE KEY UPDATE title = COALESCE"), "{s}");
        assert!(!s.contains("version ="), "{s}");
        let only_priority = create_bead_script("sp-a", 7, None, Some(0), None, None);
        assert!(only_priority.contains("VALUES ('sp-a', 'READY', NULL, '[]', 0, 7, 7, NULL, 0, NULL)"), "{only_priority}");
        let long = create_bead_script("sp-a", 7, Some(&"é".repeat(600)), None, None, None);
        assert_eq!(long.matches('é').count(), MIRROR_TITLE_CHARS, "cut at the column width by chars, not bytes");
    }

    #[test]
    fn a_submitted_create_is_born_submitted_at_its_tip() {
        let s = create_bead_script("sp-a", 7, Some("t"), Some(2), Some("abc123"), Some("spira"));
        assert!(s.contains("VALUES ('sp-a', 'SUBMITTED', 'abc123', '[]', 1, 7,"), "{s}");
        assert!(s.contains("'spira')") && s.contains("repo = COALESCE(VALUES(repo), repo)"), "{s}");
        assert!(s.contains("'Submit'") && s.contains("AND state = 'READY' AND version = 0 AND tip IS NULL"), "{s}");
        assert!(!s.contains("ON DUPLICATE KEY UPDATE state"), "a row past READY is never rewritten: {s}");
    }

    fn stacked(id: &str, prereq: &str, prereq_tip: &str) -> (String, bead::BeadRow) {
        let mut row = bead::BeadRow::filed(id);
        row.stack.insert(prereq.to_string(), prereq_tip.to_string());
        (id.to_string(), row)
    }

    fn unstacked(id: &str) -> (String, bead::BeadRow) {
        (id.to_string(), bead::BeadRow::filed(id))
    }

    /// sp-f3af9 item 4: a 3-deep stack A -> B -> C, plus an unrelated member X, ejecting the
    /// root A must cascade both B and C, never X — the BFS must revisit the frontier past
    /// B to find C rather than stopping at the first hop.
    #[test]
    fn a_three_deep_stack_cascades_root_to_tip() {
        let tips: BTreeMap<String, String> = [("a", "tipA"), ("b", "tipB"), ("c", "tipC"), ("x", "tipX")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let member_stacks = vec![unstacked("a"), stacked("b", "a", "tipA"), stacked("c", "b", "tipB"), unstacked("x")];

        let dependents = stacked_dependents_from(&member_stacks, &tips, &Default::default(), &["a".to_string()]);

        let ids: Vec<&str> = dependents.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids.len(), 2, "expected exactly B and C: {dependents:?}");
        assert!(ids.contains(&"b") && ids.contains(&"c"));
        assert!(!ids.contains(&"x"), "an unrelated member must not cascade");
        assert!(dependents.contains(&("c".to_string(), "b".to_string())));
    }

    /// A root is never returned even if some other member happens to name it as a
    /// prerequisite — cascade reports collateral, not the caller's own attribution.
    #[test]
    fn a_root_is_never_returned_as_its_own_dependent() {
        let tips: BTreeMap<String, String> = [("a".to_string(), "tipA".to_string())].into_iter().collect();
        let member_stacks = vec![unstacked("a"), stacked("b", "a", "tipA")];

        let dependents = stacked_dependents_from(&member_stacks, &tips, &Default::default(), &["a".to_string()]);

        assert_eq!(dependents, vec![("b".to_string(), "a".to_string())]);
    }

    /// The tip invariant re-check: a member whose recorded stack names a prerequisite tip
    /// that no longer matches this batch's own recorded tip for it must not cascade — that
    /// stack is already stale, not this round's own dependency.
    #[test]
    fn a_stack_naming_a_stale_tip_does_not_cascade() {
        let tips: BTreeMap<String, String> = [("a".to_string(), "tipA".to_string())].into_iter().collect();
        let member_stacks = vec![unstacked("a"), stacked("b", "a", "some-other-tip")];

        let dependents = stacked_dependents_from(&member_stacks, &tips, &Default::default(), &["a".to_string()]);

        assert!(dependents.is_empty());
    }

    #[test]
    fn the_prerequisite_is_named_in_the_returned_evidence() {
        let mut steps = vec![cascade_step("bead", "bead_id", "b", 1, String::new(), "REWORK", "bead", "Returned", "IN_DELIVERY", &serde_json::json!({"Returned": {"reason": "base-withdrawn"}}), "t", 0)];
        name_prerequisites(&mut steps, &[("b".to_string(), "a".to_string())]);
        assert_eq!(steps[0].event.evidence()["stacked_on"], "a");
        assert!(steps[0].event.evidence().get("Returned").is_some());
    }

    // ── the cascade plans: rows in, steps out, no store ──────────────────────────────────

    use super::batch::BatchState as BS;
    use super::bead::BeadState as Bd;

    const AT: i64 = 1_000;

    fn certified(id: &str, tip: &str) -> bead::BeadRow {
        let mut r = bead::BeadRow::filed(id);
        r.state = Bd::Certified;
        r.tip = Some(tip.into());
        r.gate_key = Some("k1".into());
        r.version = 3;
        r
    }

    fn submitted(id: &str, tip: &str) -> bead::BeadRow {
        let mut r = bead::BeadRow::filed(id);
        r.state = Bd::Submitted;
        r.tip = Some(tip.into());
        r.version = 2;
        r
    }

    fn stacked_on(mut r: bead::BeadRow, prereq: &str, prereq_tip: &str) -> bead::BeadRow {
        r.stack.insert(prereq.into(), prereq_tip.into());
        r
    }

    fn tipped(beads: &[bead::BeadRow]) -> Vec<(String, String)> {
        beads.iter().map(|b| (b.bead_id.clone(), b.tip.clone().unwrap())).collect()
    }

    fn roster_of(beads: &[bead::BeadRow], deliveries: &[delivery::DeliveryRow]) -> Roster {
        Roster {
            beads: beads.iter().map(|b| (b.bead_id.clone(), b.clone())).collect(),
            deliveries: deliveries.iter().map(|d| (d.bead_id.clone(), d.clone())).collect(),
        }
    }

    fn delivery_in(id: &str, state: delivery::DeliveryState, version: u64) -> delivery::DeliveryRow {
        let mut d = delivery::DeliveryRow::start_queue(id);
        d.state = state;
        d.version = version;
        d
    }

    fn cut_args(batch_id: &str, beads: &[bead::BeadRow], parent: Option<&str>) -> CutArgs {
        CutArgs {
            batch_id: batch_id.into(),
            repo: "spira".into(),
            head: "H".into(),
            base: "B".into(),
            members: tipped(beads),
            parent: parent.map(String::from),
            actor: "test".into(),
        }
    }

    /// The rows a round holds once cut, built by the machines' own `Deliver` and `Cut` events.
    fn round(state: BS, version: u64, beads: &[bead::BeadRow]) -> Rows {
        let mut batch_row = batch::BatchRow::cut("b-1", "spira", "H", "B");
        batch_row.state = state;
        batch_row.version = version;
        let mut roster = Roster::default();
        for b in beads {
            let deliver = bead::BeadEvent { expect: b.state, version: b.version, kind: bead::BeadEventKind::Deliver, actor: "t".into(), at: Some(AT) };
            let entered = bead::apply(b, &deliver).row;
            let cut = delivery::DeliveryEvent {
                expect: delivery::DeliveryState::Queued,
                version: 0,
                kind: delivery::DeliveryEventKind::Cut { batch_id: "b-1".into() },
                actor: "t".into(),
            };
            roster.deliveries.insert(b.bead_id.clone(), delivery::apply(&delivery::DeliveryRow::start_queue(&b.bead_id), &cut).row);
            roster.beads.insert(b.bead_id.clone(), entered);
        }
        Rows { batch: batch_row, members: tipped(beads), roster, contained: Default::default(), returned: Default::default() }
    }

    fn refused(r: Planned) -> Refused {
        match r {
            Err(r) => r,
            Ok(_) => panic!("expected a refusal"),
        }
    }

    fn planned(r: Planned) -> Plan {
        match r {
            Ok(p) => p,
            Err(r) => panic!("refused {}: {}", r.code, r.text),
        }
    }

    fn steps_on<'a>(p: &'a Plan, table: &str, key: &str) -> Vec<&'a CascadeStep> {
        p.steps.iter().filter(|s| s.table == table && s.key == key).collect()
    }

    fn only<'a>(p: &'a Plan, table: &str, key: &str) -> &'a CascadeStep {
        let v = steps_on(p, table, key);
        assert_eq!(v.len(), 1, "{table} {key}: {} steps", v.len());
        v[0]
    }

    fn bead_ends(p: &Plan, id: &str) -> String {
        only(p, "bead", id).applied_to_state.clone()
    }

    fn bead_reason(p: &Plan, id: &str, reason: &str) -> bool {
        only(p, "bead", id).set_clause.contains(&format!("reason = '{reason}'"))
    }

    fn settle_args(eject: &[&str], requeue: &[&str]) -> SettleArgs {
        SettleArgs {
            batch_id: "b-1".into(),
            expect: BS::Attributing,
            version: 9,
            actor: "t".into(),
            eject: eject.iter().map(|s| s.to_string()).collect(),
            requeue: requeue.iter().map(|s| s.to_string()).collect(),
        }
    }

    // cut

    #[test]
    fn a_cut_admits_a_submitted_member_beside_a_certified_one() {
        let beads = [submitted("sp-sub", "t1"), certified("sp-cer", "t2")];
        let plan = planned(plan_cut(&roster_of(&beads, &[]), &cut_args("b-1", &beads, None), AT));
        assert_eq!(plan.steps.len(), 6, "a MemberAdded, a Deliver and a Cut per member");
        assert_eq!(bead_ends(&plan, "sp-sub"), "IN_DELIVERY");
        assert_eq!(bead_ends(&plan, "sp-cer"), "IN_DELIVERY");
    }

    #[test]
    fn a_cut_still_refuses_a_rework_member() {
        let mut rw = submitted("sp-rw", "t1");
        rw.state = Bd::Rework;
        let r = refused(plan_cut(&roster_of(&[rw.clone()], &[]), &cut_args("b-1", &[rw], None), AT));
        assert_eq!(r.code, REFUSED);
        assert!(r.text.contains("sp-rw is REWORK not SUBMITTED or CERTIFIED"), "{}", r.text);
    }

    #[test]
    fn a_cut_refuses_a_member_that_is_not_certified_and_writes_no_batch_row() {
        let ready = bead::BeadRow::filed("sp-new");
        let mut args = cut_args("b-1", &[], None);
        args.members = vec![("sp-new".into(), "t1".into())];
        let r = refused(plan_cut(&roster_of(&[ready], &[]), &args, AT));
        assert_eq!(r.code, REFUSED);
        assert!(r.record.is_none(), "a refusal before any event is not an event");
    }

    #[test]
    fn a_cut_refuses_a_member_with_no_bead_row() {
        let args = CutArgs { members: vec![("sp-none".into(), "t".into())], ..cut_args("b-1", &[], None) };
        let r = refused(plan_cut(&Roster::default(), &args, AT));
        assert_eq!((r.code, r.text.as_str()), (REFUSED, "refused: no bead row for sp-none"));
    }

    #[test]
    fn a_cut_refuses_a_member_whose_certified_tip_moved() {
        let args = CutArgs { members: vec![("sp-a".into(), "other".into())], ..cut_args("b-1", &[], None) };
        let r = refused(plan_cut(&roster_of(&[certified("sp-a", "t1")], &[]), &args, AT));
        assert_eq!(r.text, "refused: sp-a's certified tip does not match other");
    }

    #[test]
    fn a_cut_over_a_member_batched_in_another_batch_is_refused_naming_it() {
        let beads = [certified("sp-bx", "t1")];
        let mut elsewhere = delivery_in("sp-bx", delivery::DeliveryState::Batched, 1);
        elsewhere.batch_id = Some("batch-elsewhere".into());
        let r = refused(plan_cut(&roster_of(&beads, &[elsewhere]), &cut_args("b-1", &beads, None), AT));
        assert_eq!(r.code, REFUSED);
        assert!(r.text.contains("batch-elsewhere"), "{}", r.text);
    }

    #[test]
    fn a_cut_over_an_exited_delivery_requeues_it_then_cuts_it_at_version_6() {
        let beads = [certified("sp-ex", "t1")];
        let plan = planned(plan_cut(&roster_of(&beads, &[delivery_in("sp-ex", delivery::DeliveryState::Exited, 4)]), &cut_args("b-1", &beads, None), AT));
        let d = steps_on(&plan, "delivery", "sp-ex");
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].event.event_name(), d[0].event.from_state(), d[0].old_version), ("Enqueued", "EXITED", 4));
        assert_eq!(d[0].applied_to_state, "QUEUED");
        assert_eq!((d[1].event.event_name(), d[1].event.from_state(), d[1].old_version), ("Cut", "QUEUED", 5));
        assert_eq!(d[1].applied_to_state, "BATCHED");
        assert!(d[1].set_clause.contains("batch_id = 'b-1'") && d[1].set_clause.contains("version = 6"), "{}", d[1].set_clause);
    }

    #[test]
    fn a_delivers_logged_from_state_is_the_beads_own() {
        let beads = [submitted("sp-sub", "t1"), certified("sp-cer", "t2")];
        let plan = planned(plan_cut(&roster_of(&beads, &[]), &cut_args("b-1", &beads, None), AT));
        let sub = only(&plan, "bead", "sp-sub");
        assert_eq!((sub.event.event_name(), sub.event.from_state(), sub.event.expect()), ("Deliver", "SUBMITTED", "SUBMITTED"));
        let cer = only(&plan, "bead", "sp-cer");
        assert_eq!((cer.event.from_state(), cer.event.expect()), ("CERTIFIED", "CERTIFIED"));
    }

    #[test]
    fn a_delivery_row_that_did_not_exist_is_enqueued_from_none_then_cut_from_queued() {
        let beads = [submitted("sp-sub", "t1")];
        let plan = planned(plan_cut(&roster_of(&beads, &[]), &cut_args("b-1", &beads, None), AT));
        assert!(plan.preamble.contains("'Enqueued', 'NONE', 'NONE'"), "{}", plan.preamble);
        let cut = only(&plan, "delivery", "sp-sub");
        assert_eq!((cut.event.event_name(), cut.event.from_state()), ("Cut", "QUEUED"));
        assert_eq!(cut.applied_to_state, "BATCHED");
    }

    #[test]
    fn a_cut_carries_its_parent_on_the_row_and_every_batch_step() {
        let beads = [certified("sp-lo", "t1"), certified("sp-hi", "t2")];
        let plan = planned(plan_cut(&roster_of(&beads, &[]), &cut_args("b-lo", &beads, Some("b-parent")), AT));
        assert!(plan.preamble.contains("'OPEN', 'b-parent'"), "{}", plan.preamble);
        let batch_steps = steps_on(&plan, "batch", "b-lo");
        assert_eq!(batch_steps.len(), 2);
        assert!(batch_steps.iter().all(|s| s.set_clause.contains("parent = 'b-parent'")), "a step wrote the constructor's NULL over the parent");
        let orphan = planned(plan_cut(&roster_of(&beads, &[]), &cut_args("b-lo", &beads, None), AT));
        assert!(orphan.preamble.contains("'OPEN', NULL"), "{}", orphan.preamble);
    }

    #[test]
    fn a_cut_of_one_member_leaves_the_batch_at_version_1() {
        let beads = [certified("sp-s1", "t1")];
        let plan = planned(plan_cut(&roster_of(&beads, &[]), &cut_args("b-1", &beads, None), AT));
        let b = only(&plan, "batch", "b-1");
        assert_eq!((b.event.event_name(), b.old_version, b.applied_to_state.as_str()), ("MemberAdded", 0, "OPEN"));
        assert!(b.set_clause.contains("version = 1"), "{}", b.set_clause);
    }

    // land

    #[test]
    fn a_land_exits_every_members_delivery_and_lands_its_bead_with_the_merge_sha() {
        let rows = round(BS::Green, 4, &[certified("sp-1", "t1"), certified("sp-2", "t2")]);
        let args = LandArgs { batch_id: "b-1".into(), expect: BS::Green, version: 4, actor: "t".into(), sha: "sha-green-1".into() };
        let plan = planned(plan_land(&rows, &args, AT));
        assert_eq!(plan.steps.len(), 5);
        let b = only(&plan, "batch", "b-1");
        assert_eq!((b.event.event_name(), b.old_version, b.applied_to_state.as_str()), ("FastForward", 4, "LANDED"));
        for id in ["sp-1", "sp-2"] {
            let d = only(&plan, "delivery", id);
            assert_eq!((d.event.event_name(), d.applied_to_state.as_str()), ("Delivered", "EXITED"));
            assert!(d.set_clause.contains("merge_sha = 'sha-green-1'"), "{}", d.set_clause);
            assert_eq!(bead_ends(&plan, id), "LANDED");
        }
        assert_eq!(plan.summary["steps"], 5);
    }

    #[test]
    fn a_land_against_a_landed_batch_is_refused_and_touches_no_member() {
        let rows = round(BS::Landed, 5, &[certified("sp-1", "t1")]);
        let args = LandArgs { batch_id: "b-1".into(), expect: BS::Green, version: 4, actor: "t".into(), sha: "sha-green-2".into() };
        let r = refused(plan_land(&rows, &args, AT));
        assert_eq!(r.code, REFUSED);
        let rec = r.record.expect("the refusal is kept as an event");
        assert_eq!((rec.event_name(), rec.from_state(), rec.expect()), ("FastForward", "LANDED", "GREEN"));
        assert!(rec.refusal().is_some());
    }

    #[test]
    fn a_land_skips_a_member_whose_delivery_left_the_batch() {
        let mut rows = round(BS::Green, 4, &[certified("sp-1", "t1"), certified("sp-2", "t2")]);
        rows.roster.deliveries.get_mut("sp-2").unwrap().state = delivery::DeliveryState::Exited;
        let args = LandArgs { batch_id: "b-1".into(), expect: BS::Green, version: 4, actor: "t".into(), sha: "s".into() };
        let plan = planned(plan_land(&rows, &args, AT));
        assert!(steps_on(&plan, "bead", "sp-2").is_empty() && steps_on(&plan, "delivery", "sp-2").is_empty());
        assert_eq!(bead_ends(&plan, "sp-1"), "LANDED");
    }

    // settle

    #[test]
    fn a_settle_returns_the_ejected_to_rework_and_certifies_the_requeued() {
        let rows = round(BS::Attributing, 9, &[certified("sp-e", "t1"), certified("sp-r1", "t2"), certified("sp-r2", "t3")]);
        let plan = planned(plan_settle(&rows, &settle_args(&["sp-e"], &["sp-r1", "sp-r2"]), AT));
        let b = only(&plan, "batch", "b-1");
        assert_eq!((b.event.event_name(), b.applied_to_state.as_str(), b.old_version), ("Settle", "SETTLED", 9));
        assert_eq!(only(&plan, "delivery", "sp-e").applied_to_state, "EXITED");
        assert_eq!(bead_ends(&plan, "sp-e"), "REWORK");
        assert!(bead_reason(&plan, "sp-e", "batch-ejected-red"));
        assert_eq!(bead_ends(&plan, "sp-r1"), "CERTIFIED");
        assert_eq!(bead_ends(&plan, "sp-r2"), "CERTIFIED");
        assert_eq!(plan.summary["base_withdrawn"], 0);
    }

    #[test]
    fn a_settle_carries_a_stacked_dependent_the_caller_requeued_into_rework() {
        let a = certified("sp-a", "tA");
        let b = stacked_on(certified("sp-b", "tB"), "sp-a", "tA");
        let x = certified("sp-x", "tX");
        let rows = round(BS::Attributing, 9, &[a, b, x]);
        let plan = planned(plan_settle(&rows, &settle_args(&["sp-a"], &["sp-b", "sp-x"]), AT));
        assert_eq!(bead_ends(&plan, "sp-a"), "REWORK");
        assert!(bead_reason(&plan, "sp-a", "batch-ejected-red"));
        assert_eq!(bead_ends(&plan, "sp-b"), "REWORK", "the dependent follows its prerequisite despite --requeue");
        assert!(bead_reason(&plan, "sp-b", "base-withdrawn"));
        assert_eq!(only(&plan, "bead", "sp-b").event.evidence()["stacked_on"], "sp-a");
        assert_eq!(bead_ends(&plan, "sp-x"), "CERTIFIED", "an unrelated member is merely requeued");
        assert_eq!(plan.summary["base_withdrawn"], 1);
        assert_eq!(plan.summary["stacked_on"]["sp-b"], "sp-a");
    }

    #[test]
    fn a_settle_walks_a_three_deep_chain_to_its_tip() {
        let a = certified("sp-a", "tA");
        let b = stacked_on(certified("sp-b", "tB"), "sp-a", "tA");
        let c = stacked_on(certified("sp-c", "tC"), "sp-b", "tB");
        let x = certified("sp-x", "tX");
        let rows = round(BS::Attributing, 9, &[a, b, c, x]);
        let plan = planned(plan_settle(&rows, &settle_args(&["sp-a"], &["sp-b", "sp-c", "sp-x"]), AT));
        assert_eq!(bead_ends(&plan, "sp-a"), "REWORK");
        for id in ["sp-b", "sp-c"] {
            assert_eq!(bead_ends(&plan, id), "REWORK", "{id}");
            assert!(bead_reason(&plan, id, "base-withdrawn"), "{id}");
        }
        assert_eq!(only(&plan, "bead", "sp-c").event.evidence()["stacked_on"], "sp-b");
        assert_eq!(bead_ends(&plan, "sp-x"), "CERTIFIED");
        assert_eq!(plan.summary["base_withdrawn"], 2);
    }

    #[test]
    fn a_settle_requeuing_every_member_returns_the_whole_pool_for_a_bisect_to_recut() {
        let beads = [certified("sp-b1", "t1"), certified("sp-b2", "t2"), certified("sp-b3", "t3"), certified("sp-b4", "t4")];
        let rows = round(BS::Attributing, 9, &beads);
        let plan = planned(plan_settle(&rows, &settle_args(&[], &["sp-b1", "sp-b2", "sp-b3", "sp-b4"]), AT));
        assert_eq!(only(&plan, "batch", "b-1").applied_to_state, "SETTLED");
        for b in &beads {
            assert_eq!(bead_ends(&plan, &b.bead_id), "CERTIFIED", "{}", b.bead_id);
            assert_eq!(only(&plan, "delivery", &b.bead_id).applied_to_state, "EXITED");
        }
    }

    #[test]
    fn a_settle_meters_each_cascaded_members_stack_depth() {
        let a = certified("sp-a", "tA");
        let mut b = stacked_on(certified("sp-b", "tB"), "sp-a", "tA");
        b.stack_depth = 1;
        let rows = round(BS::Attributing, 9, &[a, b]);
        let plan = planned(plan_settle(&rows, &settle_args(&["sp-a"], &[]), AT));
        assert_eq!(plan.summary["stack_depth"]["sp-b"], 1);
        assert_eq!(plan.summary["stack_depth"]["sp-a"], 0);
    }

    #[test]
    fn a_settle_of_a_batch_not_awaiting_attribution_is_refused_with_its_event() {
        let rows = round(BS::Open, 1, &[certified("sp-a", "tA")]);
        let r = refused(plan_settle(&rows, &settle_args(&[], &["sp-a"]), AT));
        assert_eq!(r.code, REFUSED);
        assert_eq!(r.record.expect("kept").event_name(), "Settle");
    }

    // stack

    fn stack_rows(batch_state: BS, version: u64, members: &[bead::BeadRow]) -> Rows {
        let mut batch_row = batch::BatchRow::cut("b-1", "spira", "H", "B");
        batch_row.state = batch_state;
        batch_row.version = version;
        Rows { batch: batch_row, members: Vec::new(), roster: roster_of(members, &[]), contained: Default::default(), returned: Default::default() }
    }

    fn stack_args(beads: &[bead::BeadRow]) -> StackArgs {
        StackArgs { batch_id: "b-1".into(), members: tipped(beads), actor: "t".into() }
    }

    #[test]
    fn a_stack_writes_steps_against_the_open_row_and_advances_its_version_by_the_member_count() {
        let beads = [certified("sp-s2", "t2"), certified("sp-s3", "t3")];
        let plan = planned(plan_stack(&stack_rows(BS::Open, 1, &beads), &stack_args(&beads), AT));
        assert!(!plan.preamble.contains("INTO batch ("), "no second batch row: {}", plan.preamble);
        assert_eq!(plan.preamble.matches("INTO batch_member").count(), 2);
        let b = steps_on(&plan, "batch", "b-1");
        assert_eq!((b[0].old_version, b[1].old_version), (1, 2));
        assert!(b[1].set_clause.contains("version = 3") && b[1].applied_to_state == "OPEN", "{}", b[1].set_clause);
        assert!(steps_on(&plan, "bead", "sp-s1").is_empty(), "the original member is untouched");
        assert_eq!(plan.steps.len(), 6);
    }

    #[test]
    fn a_stack_refuses_a_member_whose_tip_does_not_match_and_writes_nothing() {
        let mut args = stack_args(&[]);
        args.members = vec![("sp-s5".into(), "tip-does-not-match".into())];
        let r = refused(plan_stack(&stack_rows(BS::Open, 3, &[certified("sp-s5", "t5")]), &args, AT));
        assert_eq!(r.code, REFUSED);
        assert!(r.text.contains("does not match"), "{}", r.text);
    }

    #[test]
    fn a_stack_refuses_a_batch_that_is_settled_not_open() {
        let beads = [certified("sp-s4", "t4")];
        let r = refused(plan_stack(&stack_rows(BS::Settled, 4, &beads), &stack_args(&beads), AT));
        assert_eq!((r.code, r.text.as_str()), (REFUSED, "refused: b-1 is SETTLED not OPEN"));
    }

    #[test]
    fn a_stack_does_not_admit_a_submitted_member() {
        let beads = [submitted("sp-s6", "t6")];
        let r = refused(plan_stack(&stack_rows(BS::Open, 1, &beads), &stack_args(&beads), AT));
        assert_eq!(r.text, "refused: sp-s6 is SUBMITTED not CERTIFIED");
    }

    // stage, promote

    fn stage_args(beads: &[bead::BeadRow]) -> StageArgs {
        StageArgs { batch_id: "b-st".into(), repo: "spira".into(), head: "headG".into(), base: "baseG".into(), members: tipped(beads), parent: "b-running".into() }
    }

    #[test]
    fn a_stage_writes_a_staged_row_behind_its_parent_and_delivers_no_member() {
        let beads = [certified("sp-g1", "t1"), certified("sp-g2", "t2")];
        let plan = planned(plan_stage(&roster_of(&beads, &[]), &stage_args(&beads), AT));
        assert!(plan.preamble.contains("'STAGED', 'b-running'"), "{}", plan.preamble);
        assert_eq!(plan.preamble.matches("INTO batch_member").count(), 2);
        assert!(plan.steps.is_empty(), "a staged round that is discarded returns nothing");
    }

    #[test]
    fn a_stage_refuses_a_member_whose_tip_does_not_match_and_writes_no_row() {
        let mut args = stage_args(&[]);
        args.members = vec![("sp-g1".into(), "tip-does-not-match".into())];
        let r = refused(plan_stage(&roster_of(&[certified("sp-g1", "t1")], &[]), &args, AT));
        assert_eq!(r.code, REFUSED);
    }

    fn staged_rows(state: BS, beads: &[bead::BeadRow]) -> Rows {
        let mut batch_row = batch::BatchRow::staged("b-st", "spira", "headG", "baseG", "b-running");
        batch_row.state = state;
        Rows { batch: batch_row, members: tipped(beads), roster: roster_of(beads, &[]), contained: Default::default(), returned: Default::default() }
    }

    fn promote_args() -> PromoteArgs {
        PromoteArgs { batch_id: "b-st".into(), head: "headG2".into(), base: "baseG2".into(), actor: "t".into() }
    }

    #[test]
    fn a_promote_opens_the_round_at_the_rebuilt_head_and_delivers_every_member() {
        let beads = [certified("sp-g1", "t1"), certified("sp-g2", "t2")];
        let plan = planned(plan_promote(&staged_rows(BS::Staged, &beads), &promote_args(), AT));
        let first = &plan.steps[0];
        assert_eq!((first.table, first.event.event_name(), first.applied_to_state.as_str()), ("batch", "Promote", "OPEN"));
        assert!(first.set_clause.contains("head = 'headG2'") && first.set_clause.contains("base = 'baseG2'"), "{}", first.set_clause);
        for id in ["sp-g1", "sp-g2"] {
            assert_eq!(bead_ends(&plan, id), "IN_DELIVERY");
            assert_eq!(only(&plan, "delivery", id).applied_to_state, "BATCHED");
        }
    }

    #[test]
    fn a_promote_refuses_a_round_that_is_already_open_or_discarded() {
        let beads = [certified("sp-g1", "t1")];
        for state in [BS::Open, BS::Discarded] {
            let r = refused(plan_promote(&staged_rows(state, &beads), &promote_args(), AT));
            assert_eq!(r.code, REFUSED, "{state:?}");
            assert!(r.text.ends_with(&format!("{} not STAGED", state.as_str())), "{}", r.text);
        }
    }

    #[test]
    fn a_promote_refuses_a_member_that_moved_since_it_was_staged() {
        let mut moved = certified("sp-g1", "t1");
        moved.tip = Some("t-new".into());
        let mut rows = staged_rows(BS::Staged, &[certified("sp-g1", "t1")]);
        rows.roster = roster_of(&[moved], &[]);
        assert_eq!(refused(plan_promote(&rows, &promote_args(), AT)).code, REFUSED);
    }

    // eject-member

    fn eject_args(bead_id: &str, expect: BS, version: u64) -> EjectArgs {
        EjectArgs { batch_id: "b-1".into(), bead_id: bead_id.into(), expect, version, actor: "queue".into(), reason: "manual eject".into() }
    }

    #[test]
    fn a_hand_eject_from_open_requeues_only_the_ejected_member() {
        let rows = round(BS::Open, 2, &[certified("sp-e1", "t1"), certified("sp-e2", "t2")]);
        let plan = planned(plan_eject_member(&rows, &eject_args("sp-e1", BS::Open, 2), AT));
        let b = only(&plan, "batch", "b-1");
        assert_eq!((b.event.event_name(), b.applied_to_state.as_str()), ("Eject", "OPEN"));
        assert_eq!(bead_ends(&plan, "sp-e1"), "CERTIFIED");
        assert!(steps_on(&plan, "bead", "sp-e2").is_empty() && steps_on(&plan, "delivery", "sp-e2").is_empty(), "the survivor is untouched");
    }

    #[test]
    fn a_hand_eject_from_green_returns_the_batch_to_open() {
        let rows = round(BS::Green, 4, &[certified("sp-e2", "t2")]);
        let plan = planned(plan_eject_member(&rows, &eject_args("sp-e2", BS::Green, 4), AT));
        assert_eq!(only(&plan, "batch", "b-1").applied_to_state, "OPEN");
        assert_eq!(bead_ends(&plan, "sp-e2"), "CERTIFIED");
    }

    #[test]
    fn a_hand_eject_from_a_terminal_batch_is_refused() {
        let rows = round(BS::Abandoned, 5, &[certified("sp-e2", "t2")]);
        let r = refused(plan_eject_member(&rows, &eject_args("sp-e2", BS::Abandoned, 5), AT));
        assert_eq!(r.code, REFUSED);
        assert_eq!(r.record.expect("kept").event_name(), "Eject");
    }

    #[test]
    fn a_hand_eject_of_a_non_member_cannot_be_told() {
        let rows = round(BS::Open, 2, &[certified("sp-e1", "t1")]);
        assert_eq!(refused(plan_eject_member(&rows, &eject_args("sp-nobody", BS::Open, 2), AT)).code, CANNOT_TELL);
    }

    #[test]
    fn a_hand_eject_sends_a_stacked_dependent_to_rework_as_base_withdrawn() {
        let ha = certified("sp-ha", "tA");
        let hb = stacked_on(certified("sp-hb", "tB"), "sp-ha", "tA");
        let rows = round(BS::Open, 2, &[ha, hb]);
        let plan = planned(plan_eject_member(&rows, &eject_args("sp-ha", BS::Open, 2), AT));
        assert_eq!(only(&plan, "batch", "b-1").applied_to_state, "OPEN");
        assert_eq!(bead_ends(&plan, "sp-ha"), "CERTIFIED", "tip unchanged, no rework of its own");
        assert_eq!(bead_ends(&plan, "sp-hb"), "REWORK");
        assert!(bead_reason(&plan, "sp-hb", "base-withdrawn"));
        assert_eq!(plan.summary["base_withdrawn"], 1);
    }

    // abandon-batch

    fn abandon_args(expect: BS, version: u64) -> AbandonArgs {
        AbandonArgs { batch_id: "b-1".into(), expect, version, actor: "queue".into(), reason: "test abandon".into() }
    }

    #[test]
    fn an_abandon_returns_every_member_to_the_state_it_entered_delivery_from() {
        let rows = round(BS::Open, 2, &[submitted("sp-sub", "t1"), certified("sp-cer", "t2")]);
        let plan = planned(plan_abandon(&rows, &abandon_args(BS::Open, 2), AT));
        let b = only(&plan, "batch", "b-1");
        assert_eq!((b.event.event_name(), b.applied_to_state.as_str()), ("Abandon", "ABANDONED"));
        assert_eq!(bead_ends(&plan, "sp-sub"), "SUBMITTED", "a SUBMITTED member is never promoted");
        assert_eq!(bead_ends(&plan, "sp-cer"), "CERTIFIED");
        assert_eq!(only(&plan, "delivery", "sp-cer").applied_to_state, "EXITED");
    }

    #[test]
    fn an_abandon_keeps_the_verdict_of_a_member_the_queue_had_ejected() {
        let mut rows = round(BS::Open, 2, &[certified("sp-red", "t1")]);
        rows.returned.insert("sp-red".into());
        let plan = planned(plan_abandon(&rows, &abandon_args(BS::Open, 2), AT));
        assert_eq!(bead_ends(&plan, "sp-red"), "REWORK");
    }

    #[test]
    fn an_abandon_of_a_landed_batch_is_refused() {
        let rows = round(BS::Landed, 5, &[certified("sp-cer", "t2")]);
        assert_eq!(refused(plan_abandon(&rows, &abandon_args(BS::Landed, 5), AT)).code, REFUSED);
    }

    #[test]
    fn show_batch_of_a_batch_that_was_never_cut_exits_1_so_a_guard_fails_closed() {
        assert_eq!(show_batch_reply(None), (1, "{}".to_string()));
        let row = batch::BatchRow::cut("b-1", "spira", "H", "B");
        let (rc, out) = show_batch_reply(Some((&row, None)));
        assert_eq!(rc, 0);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!((v["state"].as_str(), v["version"].as_u64()), (Some("OPEN"), Some(0)));
    }
}

/// The delivery exit + matching bead exit for one member, appended to `steps`. `Delivered`
/// is deliberately not accepted here — `land` is the only caller with a merge_sha to hand,
/// and adding it would let a caller build a `Delivered` cascade without one.
fn exit_steps(roster: &Roster, steps: &mut Vec<CascadeStep>, id: &str, kind: delivery::DeliveryEventKind, actor: &str, at: i64) {
    let bead_kind = match &kind {
        delivery::DeliveryEventKind::Returned { reason } => bead::BeadEventKind::Returned { reason: *reason },
        delivery::DeliveryEventKind::Requeued { tip } => bead::BeadEventKind::Requeued { tip: tip.clone() },
        delivery::DeliveryEventKind::Delivered { .. }
        | delivery::DeliveryEventKind::Cut { .. }
        | delivery::DeliveryEventKind::PublishStarted { .. }
        | delivery::DeliveryEventKind::PublishCi { .. }
        | delivery::DeliveryEventKind::Published { .. }
        | delivery::DeliveryEventKind::PublishRed { .. } => {
            return; // not a settle/abandon exit shape — never reached by this module's callers
        }
    };
    let event_name = match &kind {
        delivery::DeliveryEventKind::Returned { .. } => "Returned",
        delivery::DeliveryEventKind::Requeued { .. } => "Requeued",
        _ => unreachable!(),
    };
    // A missing or already-exited delivery row never keeps the bead in IN_DELIVERY.
    if let Some(delivery_row) = roster.deliveries.get(id) {
        let ev = delivery::DeliveryEvent { expect: delivery_row.state, version: delivery_row.version, kind: kind.clone(), actor: actor.to_string() };
        let outcome = delivery::apply(delivery_row, &ev);
        if outcome.applied {
            steps.push(cascade_step(
                "delivery",
                "bead_id",
                id,
                delivery_row.version,
                rows::delivery_set_clause(&outcome.row),
                outcome.row.state.as_str(),
                "delivery",
                event_name,
                delivery_row.state.as_str(),
                &ev.kind,
                actor,
                at,
            ));
        }
    }

    let Some(bead_row) = roster.beads.get(id) else { return };
    let bead_ev = bead::BeadEvent { expect: bead_row.state, version: bead_row.version, kind: bead_kind.clone(), actor: actor.to_string(), at: Some(at) };
    let bead_outcome = bead::apply(bead_row, &bead_ev);
    if bead_outcome.applied {
        steps.push(cascade_step(
            "bead",
            "bead_id",
            id,
            bead_row.version,
            rows::bead_set_clause(&bead_outcome.row),
            bead_outcome.row.state.as_str(),
            "bead",
            event_name,
            bead_row.state.as_str(),
            &bead_kind,
            actor,
            at,
        ));
    }
}

/// `exit_steps` over the rows as the store holds them now.
fn add_exit_steps(conn: &Conn, steps: &mut Vec<CascadeStep>, id: &str, kind: delivery::DeliveryEventKind, actor: &str, at: i64) -> Result<(), DbError> {
    let roster = fetch_roster(conn, [&id.to_string()])?;
    exit_steps(&roster, steps, id, kind, actor, at);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cascade_step(
    table: &'static str,
    key_column: &'static str,
    key: &str,
    old_version: u64,
    set_clause: String,
    applied_to_state: &str,
    machine: &str,
    event_name: &str,
    from_state: &str,
    evidence_kind: &impl serde::Serialize,
    actor: &str,
    at: i64,
) -> CascadeStep {
    CascadeStep {
        table,
        key_column,
        key: key.to_string(),
        old_version,
        set_clause,
        applied_to_state: applied_to_state.to_string(),
        event: EventRecord::of_apply(machine, key, event_name, from_state, from_state, None, serde_json::to_value(evidence_kind).unwrap_or_default(), actor, at),
    }
}

/// `event-continuity`: for each (machine, lc_key), every applied event's from_state must equal
/// the previous applied event's to_state. Prints the first break per key as one JSON line.
/// A break listed in `event-continuity.acknowledged` (seq, machine, key, reason; tab-separated,
/// under the run directory) is printed with its reason and passes; any other exits 3.
/// `--acknowledge --through SEQ --reason R` records every unacknowledged break at or below SEQ.
pub fn cmd_event_continuity(args: &[String], conn: &Conn) -> (i32, String) {
    let ack_path = match crate::lifecycle_run_dir() {
        Ok(d) => d.join("event-continuity.acknowledged"),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    let mut acked = std::collections::HashMap::new();
    match std::fs::read_to_string(&ack_path) {
        Ok(t) => {
            for l in t.lines().filter(|l| !l.trim().is_empty()) {
                let f: Vec<&str> = l.splitn(4, '\t').collect();
                if f.len() != 4 {
                    return (CANNOT_TELL, format!("cannot tell: {} has a malformed row: {l}", ack_path.display()));
                }
                acked.insert((f[1].to_string(), f[2].to_string(), f[0].to_string()), f[3].to_string());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {}: {e}", ack_path.display())),
    }
    let found = match conn.live.as_ref() {
        Some(l) => crate::continuity::current(conn, l.continuity()),
        None => crate::continuity::scan(conn).map(|t| t.breaks()),
    };
    let breaks = match found {
        Ok(b) => b,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    if args.iter().any(|a| a == "--acknowledge") {
        let (Some(reason), Some(through)) = (flag(args, "--reason"), flag(args, "--through").and_then(|t| t.parse::<i64>().ok())) else {
            return (REFUSED, "refused: --acknowledge needs --reason R and --through SEQ".to_string());
        };
        if reason.trim().is_empty() || reason.contains(['\t', '\n']) {
            return (REFUSED, "refused: --reason must be one non-empty line".to_string());
        }
        let mut rows = String::new();
        for (m, k, seq, _) in &breaks {
            if seq.parse::<i64>().unwrap_or(i64::MAX) <= through && !acked.contains_key(&(m.clone(), k.clone(), seq.clone())) {
                rows.push_str(&format!("{seq}\t{m}\t{k}\t{reason}\n"));
            }
        }
        use std::io::Write;
        let wrote = std::fs::OpenOptions::new().create(true).append(true).open(&ack_path).and_then(|mut f| f.write_all(rows.as_bytes()));
        return match wrote {
            Ok(()) => (0, format!("acknowledged {} break(s)", rows.lines().count())),
            Err(e) => (CANNOT_TELL, format!("cannot tell: {}: {e}", ack_path.display())),
        };
    }
    let mut fresh = false;
    let mut lines = Vec::new();
    for (m, k, seq, mut j) in breaks {
        match acked.get(&(m, k, seq)) {
            Some(reason) => j["acknowledged"] = serde_json::Value::String(reason.clone()),
            None => fresh = true,
        }
        lines.push(j.to_string());
    }
    let out = lines.join("\n");
    if fresh { (REFUSED, out) } else { (0, out) }
}

#[cfg(test)]
mod progress_payload_tests {
    #[test]
    fn a_payload_already_inline_is_left_alone() {
        let a: Vec<String> = ["batch-progress", "b", "--json", "{}"].map(String::from).to_vec();
        assert_eq!(super::inline_stdin_payload(a.clone()), a);
    }

    #[test]
    fn stdin_travels_as_the_json_flag_and_the_handler_accepts_it() {
        let a: Vec<String> = ["batch-progress", "b"].map(String::from).to_vec();
        let out = super::inline_payload_from(a, &mut "{\"phase\":\"suites\"}\n".as_bytes());
        assert_eq!(out[2], "--json");
        assert_eq!(super::flag(&out, "--json").as_deref(), Some("{\"phase\":\"suites\"}\n"));
    }

    #[test]
    fn other_verbs_are_untouched() {
        let a: Vec<String> = ["show", "b"].map(String::from).to_vec();
        assert_eq!(super::inline_stdin_payload(a.clone()), a);
    }

    #[test]
    fn a_section_or_intent_body_travels_as_the_json_flag_and_reads_do_not_read_stdin() {
        for verb in [&["section", "apply-spec"][..], &["section", "publish-status"], &["intent", "put", "k"]] {
            let a: Vec<String> = verb.iter().map(|s| s.to_string()).collect();
            let out = super::inline_payload_from(a, &mut "{}".as_bytes());
            assert_eq!(super::flag(&out, "--json").as_deref(), Some("{}"), "{verb:?}");
        }
        for verb in [&["section", "get", "Probe/a"][..], &["intent", "ack", "k"], &["section", "snapshot"]] {
            let a: Vec<String> = verb.iter().map(|s| s.to_string()).collect();
            assert_eq!(super::inline_payload_from(a.clone(), &mut "{}".as_bytes()), a, "{verb:?}");
        }
    }

    #[test]
    fn an_rc_body_on_stdin_replaces_the_dash_and_an_inline_body_is_kept() {
        let a: Vec<String> = ["rc", "get", "--json", "-"].map(String::from).to_vec();
        let out = super::inline_payload_from(a, &mut "{\"key\":\"k\"}".as_bytes());
        assert_eq!(out, ["rc", "get", "--json", "{\"key\":\"k\"}"].map(String::from).to_vec());
        let a: Vec<String> = ["rc", "get", "--json", "{}"].map(String::from).to_vec();
        assert_eq!(super::inline_payload_from(a.clone(), &mut "ignored".as_bytes()), a);
    }
}
