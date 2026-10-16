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
        // Built field-by-field, not a derive-serialize of BatchRow: BatchState's derived
        // Serialize prints the Rust variant name ("Open"), while every `--expect`/`state`
        // string elsewhere in this CLI (and the value a caller must feed back into
        // `--expect`) is `.as_str()`'s upper-snake form ("OPEN"). Two spellings of the same
        // state would make a caller's own string compare silently always fail.
        Ok(Some(row)) => {
            let progress = match conn.query(&format!("SELECT progress FROM batch WHERE batch_id = {}", q(batch_id))) {
                Ok(r) => r.first().and_then(|r| r.get("progress")).and_then(|p| p.as_str()).and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok()),
                Err(e) => return cannot_tell(e),
            };
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
        Ok(None) => (1, "{}".to_string()),
        Err(e) => cannot_tell(e),
    }
}

/// The service never sees this process's stdin, so `batch-progress`'s payload is read here and
/// travels as `--json <body>`.
pub fn inline_stdin_payload(args: Vec<String>) -> Vec<String> {
    inline_payload_from(args, &mut std::io::stdin())
}

fn inline_payload_from(mut args: Vec<String>, input: &mut dyn std::io::Read) -> Vec<String> {
    if args.first().map(String::as_str) == Some("batch-progress") && !args.iter().any(|a| a == "--json") {
        let mut body = String::new();
        let _ = input.read_to_string(&mut body);
        args.push("--json".into());
        args.push(body);
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

/// `cut <batch-id> --repo R --head H --base B --members "id:tip,id:tip" --actor A [--parent P]`
///
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
    if let Err(early) = fetch_certified_members(conn, &members, true) {
        return early;
    }
    let at = crate::db::now_epoch();
    let mut preamble = format!(
        "INSERT INTO batch (batch_id, repo, state, parent, head, base, run, version, opened_at)\n\
         VALUES ({batch_id}, {repo}, 'STAGED', {parent}, {head}, {base}, NULL, 0, {at});\n",
        batch_id = q(batch_id),
        repo = q(&repo),
        parent = q(&parent),
        head = q(&head),
        base = q(&base),
    );
    for (id, tip) in &members {
        preamble.push_str(&format!(
            "INSERT INTO batch_member (batch_id, bead_id, tip) VALUES ({}, {}, {});\n",
            q(batch_id),
            q(id),
            q(tip),
        ));
    }
    match conn.cascade(&preamble, &[]) {
        Ok(_) => (0, serde_json::json!({"batch_id": batch_id, "members": members.iter().map(|(i, _)| i).collect::<Vec<_>>()}).to_string()),
        Err(e) => cannot_tell(e),
    }
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
    if batch_row.state != batch::BatchState::Staged {
        return (REFUSED, format!("refused: {batch_id} is {} not STAGED", batch_row.state.as_str()));
    }
    let members = match fetch_members(conn, batch_id) {
        Ok(m) => m,
        Err(e) => return cannot_tell(e),
    };
    let bead_rows = match fetch_certified_members(conn, &members, true) {
        Ok(r) => r,
        Err(early) => return early,
    };
    let delivery_rows = match fetch_cuttable_deliveries(conn, &members) {
        Ok(r) => r,
        Err(early) => return early,
    };
    let at = crate::db::now_epoch();
    let promote_ev = batch::BatchEvent {
        expect: batch::BatchState::Staged,
        version: batch_row.version,
        kind: batch::BatchEventKind::Promote { head, base },
        actor: actor.clone(),
    };
    let outcome = batch::apply(&batch_row, &promote_ev);
    if !outcome.applied {
        return (REFUSED, format!("refused: {:?}", outcome.refusal));
    }
    let mut steps = vec![CascadeStep {
        table: "batch",
        key_column: "batch_id",
        key: batch_id.clone(),
        old_version: batch_row.version,
        set_clause: rows::batch_set_clause(&outcome.row),
        applied_to_state: outcome.row.state.as_str().to_string(),
        event: EventRecord::of_apply("batch", batch_id, "Promote", "STAGED", batch_row.state.as_str(), None, serde_json::to_value(&promote_ev.kind).unwrap_or_default(), &actor, at),
    }];
    let mut preamble = String::new();
    match member_added_steps(batch_id, outcome.row, &members, &bead_rows, &delivery_rows, &actor, at, &mut preamble, "promote") {
        Ok(more) => steps.extend(more),
        Err(early) => return early,
    }
    match conn.cascade(&preamble, &steps) {
        Ok(applied) => {
            let summary = serde_json::json!({"batch_id": batch_id, "members": members.iter().map(|(i, _)| i).collect::<Vec<_>>(), "applied": applied});
            (if applied.iter().all(|a| *a) { 0 } else { REFUSED }, summary.to_string())
        }
        Err(e) => cannot_tell(e),
    }
}

/// Every named member must currently be CERTIFIED at exactly the given tip — checked before
/// anything is written, so a stale caller refuses cleanly instead of cutting a batch with
/// the wrong content. On success, in one transaction: the batch row is created (OPEN), one
/// `batch_member` row and one `MemberAdded` batch event per member, the bead's `Deliver`
/// event (CERTIFIED -> IN_DELIVERY), and the delivery row's `Cut` event (fresh -> BATCHED).
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

    let bead_rows = match fetch_certified_members(conn, &members, true) {
        Ok(r) => r,
        Err(early) => return early,
    };
    let delivery_rows = match fetch_cuttable_deliveries(conn, &members) {
        Ok(r) => r,
        Err(early) => return early,
    };

    let at = crate::db::now_epoch();
    let mut preamble = format!(
        "INSERT INTO batch (batch_id, repo, state, parent, head, base, run, version, opened_at)\n\
         VALUES ({batch_id}, {repo}, 'OPEN', {parent}, {head}, {base}, NULL, 0, {at});\n",
        batch_id = q(batch_id),
        repo = q(&repo),
        parent = parent.as_deref().map(q).unwrap_or_else(|| "NULL".into()),
        head = q(&head),
        base = q(&base),
        at = at,
    );
    for (id, tip) in &members {
        preamble.push_str(&format!(
            "INSERT INTO batch_member (batch_id, bead_id, tip) VALUES ({batch_id}, {id}, {tip});\n",
            batch_id = q(batch_id),
            id = q(id),
            tip = q(tip),
        ));
    }

    let mut batch_row = batch::BatchRow::cut(batch_id.as_str(), repo.as_str(), head.as_str(), base.as_str());
    // `cut()` always sets `parent: None` — it has no way to know this is a bisect child.
    // Patch it in before the first `MemberAdded` clones this row, so every subsequent
    // cascade step's SET clause (built from that clone) keeps carrying it forward instead
    // of writing the constructor's NULL back over the preamble's own INSERT.
    batch_row.parent = parent.clone();
    let steps = match member_added_steps(batch_id, batch_row, &members, &bead_rows, &delivery_rows, &actor, at, &mut preamble, "cut") {
        Ok(s) => s,
        Err(early) => return early,
    };

    match conn.cascade(&preamble, &steps) {
        Ok(applied) => {
            let all_ok = applied.iter().all(|a| *a);
            let summary = serde_json::json!({"batch_id": batch_id, "members": members.iter().map(|(i,_)| i).collect::<Vec<_>>(), "applied": applied});
            if all_ok {
                (0, serde_json::to_string(&summary).unwrap())
            } else {
                (REFUSED, serde_json::to_string(&summary).unwrap())
            }
        }
        Err(e) => cannot_tell(e),
    }
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
    if batch_row.state != batch::BatchState::Open {
        return (REFUSED, format!("refused: {batch_id} is {} not OPEN", batch_row.state.as_str()));
    }

    let bead_rows = match fetch_certified_members(conn, &members, false) {
        Ok(r) => r,
        Err(early) => return early,
    };
    let delivery_rows = match fetch_cuttable_deliveries(conn, &members) {
        Ok(r) => r,
        Err(early) => return early,
    };

    let at = crate::db::now_epoch();
    let mut preamble = String::new();
    for (id, tip) in &members {
        preamble.push_str(&format!(
            "INSERT INTO batch_member (batch_id, bead_id, tip) VALUES ({batch_id}, {id}, {tip});\n",
            batch_id = q(batch_id),
            id = q(id),
            tip = q(tip),
        ));
    }

    let steps = match member_added_steps(batch_id, batch_row, &members, &bead_rows, &delivery_rows, &actor, at, &mut preamble, "stack") {
        Ok(s) => s,
        Err(early) => return early,
    };

    match conn.cascade(&preamble, &steps) {
        Ok(applied) => {
            let all_ok = applied.iter().all(|a| *a);
            let summary = serde_json::json!({"batch_id": batch_id, "members": members.iter().map(|(i,_)| i).collect::<Vec<_>>(), "applied": applied});
            if all_ok {
                (0, serde_json::to_string(&summary).unwrap())
            } else {
                (REFUSED, serde_json::to_string(&summary).unwrap())
            }
        }
        Err(e) => cannot_tell(e),
    }
}

/// Every named member must currently be CERTIFIED at exactly the given tip — checked
/// before anything is written, so a stale caller (`cut` or `stack` alike) refuses
/// cleanly instead of admitting a member with the wrong content.
/// `cut` passes `allow_submitted`: a round takes SUBMITTED beads directly, its own full suite
/// being the certification (law-a-round-is-feature-first-then-catch-all); `stack` does not.
fn fetch_certified_members(conn: &Conn, members: &[(String, String)], allow_submitted: bool) -> Result<Vec<bead::BeadRow>, (i32, String)> {
    let mut bead_rows = Vec::new();
    for (id, tip) in members {
        let row = match rows::fetch_bead(conn, id) {
            Ok(Some(r)) => r,
            Ok(None) => return Err((REFUSED, format!("refused: no bead row for {id}"))),
            Err(e) => return Err(cannot_tell(e)),
        };
        let admitted = row.state == bead::BeadState::Certified || (allow_submitted && row.state == bead::BeadState::Submitted);
        if !admitted {
            let want = if allow_submitted { "SUBMITTED or CERTIFIED" } else { "CERTIFIED" };
            return Err((REFUSED, format!("refused: {id} is {} not {want}", row.state.as_str())));
        }
        if row.tip.as_deref() != Some(tip.as_str()) {
            return Err((REFUSED, format!("refused: {id}'s certified tip does not match {tip}")));
        }
        bead_rows.push(row);
    }
    Ok(bead_rows)
}

/// Each member's existing delivery row, or None. A row still BATCHED in another batch is
/// that batch's member: the cut refuses, naming the batch. Only a QUEUED or EXITED row can
/// be cut.
fn fetch_cuttable_deliveries(conn: &Conn, members: &[(String, String)]) -> Result<Vec<Option<delivery::DeliveryRow>>, (i32, String)> {
    let mut out = Vec::new();
    for (id, _) in members {
        let row = match rows::fetch_delivery(conn, id) {
            Ok(r) => r,
            Err(e) => return Err(cannot_tell(e)),
        };
        if let Some(r) = &row {
            match r.state {
                delivery::DeliveryState::Queued | delivery::DeliveryState::Exited => {}
                delivery::DeliveryState::Batched => {
                    return Err((REFUSED, format!("refused: {id} is still BATCHED in {}", r.batch_id.as_deref().unwrap_or("another batch"))));
                }
                s => return Err((REFUSED, format!("refused: {id}'s delivery is {} and cannot be cut", s.as_str()))),
            }
        }
        out.push(row);
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
            event: EventRecord::of_apply("bead", &id, "Deliver", bead_row.state.as_str(), bead_row.state.as_str(), None, serde_json::to_value(&deliver_ev.kind).unwrap_or_default(), actor, at),
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
    let at = crate::db::now_epoch();
    let ff_ev = batch::BatchEvent {
        expect: expect_state,
        version,
        kind: batch::BatchEventKind::FastForward { sha: sha.clone() },
        actor: actor.clone(),
    };
    let outcome = batch::apply(&batch_row, &ff_ev);
    let evidence = serde_json::to_value(&ff_ev.kind).unwrap_or_default();
    if !outcome.applied {
        let rec = EventRecord::of_apply("batch", &batch_id, "FastForward", &expect, batch_row.state.as_str(), outcome.refusal.as_ref().map(refusal_name), evidence, &actor, at);
        return match conn.insert_refusal_event(&rec) {
            Ok(()) => (REFUSED, format!("refused: {:?}", outcome.refusal)),
            Err(e) => cannot_tell(e),
        };
    }

    let members = match fetch_members(conn, batch_id) {
        Ok(m) => m,
        Err(e) => return cannot_tell(e),
    };

    let mut steps = vec![CascadeStep {
        table: "batch",
        key_column: "batch_id",
        key: batch_id.clone(),
        old_version: version,
        set_clause: rows::batch_set_clause(&outcome.row),
        applied_to_state: outcome.row.state.as_str().to_string(),
        event: EventRecord::of_apply("batch", &batch_id, "FastForward", &expect, batch_row.state.as_str(), None, evidence, &actor, at),
    }];

    for (id, tip) in &members {
        let Some(delivery_row) = (match rows::fetch_delivery(conn, id) {
            Ok(r) => r,
            Err(e) => return cannot_tell(e),
        }) else {
            continue;
        };
        let delivered_ev = delivery::DeliveryEvent {
            expect: delivery_row.state,
            version: delivery_row.version,
            kind: delivery::DeliveryEventKind::Delivered { merge_sha: sha.clone(), proof: format!("queue-fast-forward:{tip}") },
            actor: actor.clone(),
        };
        let d_outcome = delivery::apply(&delivery_row, &delivered_ev);
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
                &actor,
                at,
            ));
        }

        let Some(bead_row) = (match rows::fetch_bead(conn, id) {
            Ok(r) => r,
            Err(e) => return cannot_tell(e),
        }) else {
            continue;
        };
        let bead_delivered_ev = bead::BeadEvent {
            expect: bead_row.state,
            version: bead_row.version,
            kind: bead::BeadEventKind::Delivered { merge_sha: sha.clone(), proof: format!("queue-fast-forward:{tip}") },
            actor: actor.clone(),
            at: Some(at),
        };
        let b_outcome = bead::apply(&bead_row, &bead_delivered_ev);
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
                &actor,
                at,
            ));
        }
    }

    match conn.cascade("", &steps) {
        Ok(applied) => {
            let all_ok = applied.iter().all(|a| *a);
            let summary = serde_json::json!({"batch_id": batch_id, "steps": steps.len(), "applied": applied});
            if all_ok {
                (0, serde_json::to_string(&summary).unwrap())
            } else {
                (REFUSED, serde_json::to_string(&summary).unwrap())
            }
        }
        Err(e) => cannot_tell(e),
    }
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
    let eject_ids = parse_ids(&flag(args, "--eject").unwrap_or_default());
    let requeue_ids = parse_ids(&flag(args, "--requeue").unwrap_or_default());

    let batch_row = match rows::fetch_batch(conn, batch_id) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("settle: no batch row for {batch_id}")),
        Err(e) => return cannot_tell(e),
    };
    let at = crate::db::now_epoch();
    let settle_ev = batch::BatchEvent { expect: expect_state, version, kind: batch::BatchEventKind::Settle, actor: actor.clone() };
    let outcome = batch::apply(&batch_row, &settle_ev);
    if !outcome.applied {
        let rec = EventRecord::of_apply("batch", &batch_id, "Settle", &expect, batch_row.state.as_str(), outcome.refusal.as_ref().map(refusal_name), serde_json::Value::String("Settle".into()), &actor, at);
        return match conn.insert_refusal_event(&rec) {
            Ok(()) => (REFUSED, format!("refused: {:?}", outcome.refusal)),
            Err(e) => cannot_tell(e),
        };
    }

    let mut steps = vec![CascadeStep {
        table: "batch",
        key_column: "batch_id",
        key: batch_id.clone(),
        old_version: version,
        set_clause: rows::batch_set_clause(&outcome.row),
        applied_to_state: outcome.row.state.as_str().to_string(),
        event: EventRecord::of_apply("batch", &batch_id, "Settle", &expect, batch_row.state.as_str(), None, serde_json::Value::String("Settle".into()), &actor, at),
    }];

    // Closure (design stacked-dependents-2026-09-28 §3): ejecting a member ejects every
    // member of this same round stacked on it, transitively. Attribution names only the
    // red member(s) in `--eject`; this is what makes its dependents follow automatically,
    // regardless of which member attribution blamed.
    let cascade: Vec<(String, String)> = match stacked_dependents_in_batch(conn, &batch_row, &eject_ids) {
        Ok(ids) => ids.into_iter().filter(|(id, _)| !eject_ids.contains(id)).collect(),
        Err(e) => return cannot_tell(e),
    };
    let cascade_ids: Vec<String> = cascade.iter().map(|(id, _)| id.clone()).collect();
    let requeue_ids: Vec<String> =
        requeue_ids.into_iter().filter(|id| !cascade_ids.contains(id) && !eject_ids.contains(id)).collect();

    for id in &eject_ids {
        if let Err(e) = add_exit_steps(conn, &mut steps, id, delivery::DeliveryEventKind::Returned { reason: lifecycle::reason::ReturnedReason::BatchEjectedRed }, &actor, at) { return cannot_tell(e); }
    }
    // A stacked dependent is collateral, not itself red: `base-withdrawn`, not
    // `batch-ejected`, so the reconciler's rework-by-cause query can tell them apart.
    for id in &cascade_ids {
        if let Err(e) = add_exit_steps(conn, &mut steps, id, delivery::DeliveryEventKind::Returned { reason: lifecycle::reason::ReturnedReason::BaseWithdrawn }, &actor, at) { return cannot_tell(e); }
    }
    name_prerequisites(&mut steps, &cascade);
    for id in &requeue_ids {
        let tip = match fetch_members(conn, batch_id) {
            Ok(m) => m.into_iter().find(|(mid, _)| mid == id).map(|(_, t)| t),
            Err(e) => return cannot_tell(e),
        };
        let Some(tip) = tip else { continue };
        if let Err(e) = add_exit_steps(conn, &mut steps, id, delivery::DeliveryEventKind::Requeued { tip }, &actor, at) { return cannot_tell(e); }
    }

    match conn.cascade("", &steps) {
        Ok(applied) => {
            let all_ok = applied.iter().all(|a| *a);
            // The meter (design §3: "a stack_depth and base_withdrawn count per round"):
            // how many members this settle carried down as collateral, and how deep each
            // ejected/cascaded member's own stack was.
            let mut stack_depth = serde_json::Map::new();
            for id in eject_ids.iter().chain(cascade_ids.iter()) {
                if let Ok(Some(row)) = rows::fetch_bead(conn, id) {
                    stack_depth.insert(id.clone(), serde_json::json!(row.stack_depth));
                }
            }
            let summary = serde_json::json!({
                "batch_id": batch_id,
                "steps": steps.len(),
                "applied": applied,
                "base_withdrawn": cascade_ids.len(),
                "stacked_on": cascade.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
                "stack_depth": stack_depth,
            });
            if all_ok {
                (0, serde_json::to_string(&summary).unwrap())
            } else {
                (REFUSED, serde_json::to_string(&summary).unwrap())
            }
        }
        Err(e) => cannot_tell(e),
    }
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
fn release_kind(conn: &Conn, id: &str, tip: String) -> Result<delivery::DeliveryEventKind, DbError> {
    Ok(if returned_since_delivery(conn, id)? {
        delivery::DeliveryEventKind::Returned { reason: lifecycle::reason::ReturnedReason::BatchEjectedRed }
    } else {
        delivery::DeliveryEventKind::Requeued { tip }
    })
}

/// `abandon-batch <batch-id> --expect S --version V --actor A --reason R`
///
/// One transaction: `Abandon{reason}` on the batch (any non-terminal S -> ABANDONED), and
/// `Requeued` for every member's delivery and bead row (tip unchanged, so every one returns
/// to CERTIFIED).
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
    let at = crate::db::now_epoch();
    let ev = batch::BatchEvent { expect: expect_state, version, kind: batch::BatchEventKind::Abandon { reason: reason.clone() }, actor: actor.clone() };
    let outcome = batch::apply(&batch_row, &ev);
    let evidence = serde_json::to_value(&ev.kind).unwrap_or_default();
    if !outcome.applied {
        let rec = EventRecord::of_apply("batch", &batch_id, "Abandon", &expect, batch_row.state.as_str(), outcome.refusal.as_ref().map(refusal_name), evidence, &actor, at);
        return match conn.insert_refusal_event(&rec) {
            Ok(()) => (REFUSED, format!("refused: {:?}", outcome.refusal)),
            Err(e) => cannot_tell(e),
        };
    }

    let members = match fetch_members(conn, batch_id) {
        Ok(m) => m,
        Err(e) => return cannot_tell(e),
    };

    let mut steps = vec![CascadeStep {
        table: "batch",
        key_column: "batch_id",
        key: batch_id.clone(),
        old_version: version,
        set_clause: rows::batch_set_clause(&outcome.row),
        applied_to_state: outcome.row.state.as_str().to_string(),
        event: EventRecord::of_apply("batch", &batch_id, "Abandon", &expect, batch_row.state.as_str(), None, evidence, &actor, at),
    }];

    for (id, tip) in &members {
        let kind = match release_kind(conn, id, tip.clone()) {
            Ok(k) => k,
            Err(e) => return cannot_tell(e),
        };
        if let Err(e) = add_exit_steps(conn, &mut steps, id, kind, &actor, at) {
            return cannot_tell(e);
        }
    }

    match conn.cascade("", &steps) {
        Ok(applied) => {
            let all_ok = applied.iter().all(|a| *a);
            let summary = serde_json::json!({"batch_id": batch_id, "steps": steps.len(), "applied": applied});
            if all_ok {
                (0, serde_json::to_string(&summary).unwrap())
            } else {
                (REFUSED, serde_json::to_string(&summary).unwrap())
            }
        }
        Err(e) => cannot_tell(e),
    }
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
    let members = match fetch_members(conn, batch_id) {
        Ok(m) => m,
        Err(e) => return cannot_tell(e),
    };
    let Some(tip) = members.iter().find(|(id, _)| id == &bead_id).map(|(_, t)| t.clone()) else {
        return (CANNOT_TELL, format!("eject-member: {bead_id} is not a member of {batch_id}"));
    };

    let at = crate::db::now_epoch();
    let ev = batch::BatchEvent {
        expect: expect_state,
        version,
        kind: batch::BatchEventKind::Eject { bead_id: bead_id.clone(), reason: reason.clone() },
        actor: actor.clone(),
    };
    let outcome = batch::apply(&batch_row, &ev);
    let evidence = serde_json::to_value(&ev.kind).unwrap_or_default();
    if !outcome.applied {
        let rec = EventRecord::of_apply("batch", &batch_id, "Eject", &expect, batch_row.state.as_str(), outcome.refusal.as_ref().map(refusal_name), evidence, &actor, at);
        return match conn.insert_refusal_event(&rec) {
            Ok(()) => (REFUSED, format!("refused: {:?}", outcome.refusal)),
            Err(e) => cannot_tell(e),
        };
    }

    let mut steps = vec![CascadeStep {
        table: "batch",
        key_column: "batch_id",
        key: batch_id.clone(),
        old_version: version,
        set_clause: rows::batch_set_clause(&outcome.row),
        applied_to_state: outcome.row.state.as_str().to_string(),
        event: EventRecord::of_apply("batch", &batch_id, "Eject", &expect, batch_row.state.as_str(), None, evidence, &actor, at),
    }];

    if let Err(e) = add_exit_steps(conn, &mut steps, &bead_id, delivery::DeliveryEventKind::Requeued { tip }, &actor, at) {
        return cannot_tell(e);
    }

    // A hand eject cascades exactly like a settle's own attribution does (design §3): every
    // member of this round stacked on `bead_id`, transitively, follows it out as collateral
    // rework — `base-withdrawn`, never `batch-ejected`, since this member's own content was
    // never accused of anything.
    let cascade: Vec<(String, String)> = match stacked_dependents_in_batch(conn, &batch_row, std::slice::from_ref(&bead_id)) {
        Ok(ids) => ids,
        Err(e) => return cannot_tell(e),
    };
    let cascade_ids: Vec<String> = cascade.iter().map(|(id, _)| id.clone()).collect();
    for id in &cascade_ids {
        if let Err(e) = add_exit_steps(conn, &mut steps, id, delivery::DeliveryEventKind::Returned { reason: lifecycle::reason::ReturnedReason::BaseWithdrawn }, &actor, at) {
            return cannot_tell(e);
        }
    }
    name_prerequisites(&mut steps, &cascade);

    match conn.cascade("", &steps) {
        Ok(applied) => {
            let all_ok = applied.iter().all(|a| *a);
            let summary = serde_json::json!({
                "batch_id": batch_id,
                "bead_id": bead_id,
                "steps": steps.len(),
                "applied": applied,
                "base_withdrawn": cascade_ids.len(),
                "stacked_on": cascade.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
            });
            if all_ok {
                (0, serde_json::to_string(&summary).unwrap())
            } else {
                (REFUSED, serde_json::to_string(&summary).unwrap())
            }
        }
        Err(e) => cannot_tell(e),
    }
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
fn stacked_dependents_in_batch(conn: &Conn, batch_row: &batch::BatchRow, roots: &[String]) -> Result<Vec<(String, String)>, DbError> {
    let members = fetch_members(conn, &batch_row.batch_id)?;
    let tips: std::collections::BTreeMap<String, String> = members.iter().cloned().collect();

    let mut member_stacks = Vec::new();
    for (id, _) in &members {
        if let Some(row) = rows::fetch_bead(conn, id)? {
            member_stacks.push((id.clone(), row));
        }
    }

    let contained = branch_containment(batch_row, &members);
    Ok(stacked_dependents_from(&member_stacks, &tips, &contained, roots))
}

/// `(dependent, prerequisite)` for every pair of members where the dependent's branch holds a
/// commit in the prerequisite's own range above the batch's base. A repository, base or tip
/// git cannot resolve yields no edge: the declared `stack` still applies.
fn branch_containment(batch_row: &batch::BatchRow, members: &[(String, String)]) -> std::collections::BTreeSet<(String, String)> {
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
fn stacked_dependents_from(
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

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// B's branch is built on A's commit with no `stack` declared anywhere: ejecting A must
    /// still take B, and name A; an unrelated X stays.
    #[test]
    fn a_branch_containing_the_ejected_members_commit_cascades_without_a_declared_stack() {
        let dir = testkit::TempDir::new("spira-lc-containment");
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "t@example.invalid"]);
        git(&dir, &["config", "user.name", "t"]);
        let commit = |f: &str| {
            std::fs::write(dir.join(f), f).unwrap();
            git(&dir, &["add", f]);
            git(&dir, &["commit", "-q", "-m", f]);
            git(&dir, &["rev-parse", "HEAD"])
        };
        let base = commit("base");
        git(&dir, &["checkout", "-q", "-b", "a"]);
        let tip_a = commit("a");
        git(&dir, &["checkout", "-q", "-b", "b"]);
        let tip_b = commit("b");
        git(&dir, &["checkout", "-q", "-b", "x", &base]);
        let tip_x = commit("x");

        let members = vec![("a".to_string(), tip_a.clone()), ("b".to_string(), tip_b.clone()), ("x".to_string(), tip_x.clone())];
        let tips: BTreeMap<String, String> = members.iter().cloned().collect();
        let contained = containment_in_repo(&dir, &base, &members);
        let member_stacks = vec![unstacked("a"), unstacked("b"), unstacked("x")];

        let dependents = stacked_dependents_from(&member_stacks, &tips, &contained, &["a".to_string()]);
        assert_eq!(dependents, vec![("b".to_string(), "a".to_string())]);
        assert!(stacked_dependents_from(&member_stacks, &tips, &contained, &["x".to_string()]).is_empty());
        assert!(stacked_dependents_from(&member_stacks, &tips, &contained, &["b".to_string()]).is_empty());
    }

    #[test]
    fn the_prerequisite_is_named_in_the_returned_evidence() {
        let mut steps = vec![cascade_step("bead", "bead_id", "b", 1, String::new(), "REWORK", "bead", "Returned", "IN_DELIVERY", &serde_json::json!({"Returned": {"reason": "base-withdrawn"}}), "t", 0)];
        name_prerequisites(&mut steps, &[("b".to_string(), "a".to_string())]);
        assert_eq!(steps[0].event.evidence()["stacked_on"], "a");
        assert!(steps[0].event.evidence().get("Returned").is_some());
    }
}

/// The delivery exit + matching bead exit for one member, appended to `steps`. `Delivered`
/// is deliberately not accepted here — `land` is the only caller with a merge_sha to hand,
/// and adding it would let a caller build a `Delivered` cascade without one.
fn add_exit_steps(
    conn: &Conn,
    steps: &mut Vec<CascadeStep>,
    id: &str,
    kind: delivery::DeliveryEventKind,
    actor: &str,
    at: i64,
) -> Result<(), DbError> {
    let bead_kind = match &kind {
        delivery::DeliveryEventKind::Returned { reason } => bead::BeadEventKind::Returned { reason: reason.clone() },
        delivery::DeliveryEventKind::Requeued { tip } => bead::BeadEventKind::Requeued { tip: tip.clone() },
        delivery::DeliveryEventKind::Delivered { .. }
        | delivery::DeliveryEventKind::Cut { .. }
        | delivery::DeliveryEventKind::PublishStarted { .. }
        | delivery::DeliveryEventKind::PublishCi { .. }
        | delivery::DeliveryEventKind::Published { .. }
        | delivery::DeliveryEventKind::PublishRed { .. } => {
            return Ok(()); // not a settle/abandon exit shape — never reached by this module's callers
        }
    };
    let event_name = match &kind {
        delivery::DeliveryEventKind::Returned { .. } => "Returned",
        delivery::DeliveryEventKind::Requeued { .. } => "Requeued",
        _ => unreachable!(),
    };
    // A missing or already-exited delivery row never keeps the bead in IN_DELIVERY.
    if let Some(delivery_row) = rows::fetch_delivery(conn, id)? {
        let ev = delivery::DeliveryEvent { expect: delivery_row.state, version: delivery_row.version, kind: kind.clone(), actor: actor.to_string() };
        let outcome = delivery::apply(&delivery_row, &ev);
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

    let Some(bead_row) = rows::fetch_bead(conn, id)? else { return Ok(()) };
    let bead_ev = bead::BeadEvent { expect: bead_row.state, version: bead_row.version, kind: bead_kind.clone(), actor: actor.to_string(), at: Some(at) };
    let bead_outcome = bead::apply(&bead_row, &bead_ev);
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
/// The first run records the highest break seq in `event-continuity.baseline` under the run
/// directory and exits 0; later runs exit 3 only for a break past that mark.
pub fn cmd_event_continuity(_args: &[String], conn: &Conn) -> (i32, String) {
    let baseline_path = match crate::lifecycle_run_dir() {
        Ok(d) => d.join("event-continuity.baseline"),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    let baseline: Option<i64> = match std::fs::read_to_string(&baseline_path) {
        Ok(t) => match t.trim().parse() {
            Ok(n) => Some(n),
            Err(_) => return (CANNOT_TELL, format!("cannot tell: {} is not a seq", baseline_path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {}: {e}", baseline_path.display())),
    };
    let sql = "SELECT machine, lc_key, seq, event, from_state, prev_to FROM (\
               SELECT seq, machine, lc_key, event, from_state, \
               LAG(to_state) OVER (PARTITION BY machine, lc_key ORDER BY seq) AS prev_to \
               FROM event WHERE applied = 1) t \
               WHERE prev_to IS NOT NULL AND from_state <> prev_to ORDER BY seq";
    let rows = match conn.query(sql) {
        Ok(r) => r,
        Err(e) => return cannot_tell(e),
    };
    let mut seen = std::collections::HashSet::new();
    let mut breaks = Vec::new();
    let mut high = 0i64;
    let mut fresh = false;
    for r in rows {
        let field = |c: &str| r.get(c).and_then(|v| v.as_str()).unwrap_or("").to_string();
        if seen.insert((field("machine"), field("lc_key"))) {
            let seq: i64 = field("seq").parse().unwrap_or(0);
            high = high.max(seq);
            fresh |= baseline.is_some_and(|b| seq > b);
            breaks.push(serde_json::json!({
                "machine": field("machine"), "key": field("lc_key"), "seq": field("seq"),
                "event": field("event"), "from_state": field("from_state"), "previous_to_state": field("prev_to"),
            }));
        }
    }
    let out = breaks.iter().map(|b| b.to_string()).collect::<Vec<_>>().join("\n");
    if baseline.is_none() {
        if let Err(e) = std::fs::write(&baseline_path, high.to_string()) {
            return (CANNOT_TELL, format!("cannot tell: {}: {e}", baseline_path.display()));
        }
    }
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
}
