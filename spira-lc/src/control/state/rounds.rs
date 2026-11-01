//! The round's verbs on the control store, and the writes no machine event carries, each
//! answering as its dolt path does: `cut`, `land`, `event batch|delivery`, `show-batch`,
//! `batches-of`, `batch-progress`, `claim-verdicts`, `set-repo`, `dep-add`, `dep-remove` and
//! `delete-bead-row`.

use lifecycle::batch::{BatchEvent, BatchEventKind, BatchState};
use lifecycle::delivery::{DeliveryEvent, DeliveryEventKind, DeliveryRow, DeliveryState};
use serde_json::{json, Value};
use state_store::{Backend, Request};

use super::{cannot_tell, number, Call};
use crate::control::store::{Cut, Edge, Ran, RepoSet, Store, Turned, Verdict};
use crate::{flag, CANNOT_TELL, REFUSED};

/// The verbs this module answers.
pub const VERBS: &[&str] = &[
    "cut",
    "land",
    "show-batch",
    "batches-of",
    "batch-progress",
    "claim-verdicts",
    "set-repo",
    "dep-add",
    "dep-remove",
    "delete-bead-row",
];

pub fn run<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let rest = &args[1..];
    match args.first().map(String::as_str) {
        Some("cut") => cut(store, rest, call),
        Some("land") => land(store, rest, call),
        Some("show-batch") => show_batch(store, rest),
        Some("batches-of") => batches_of(store, rest),
        Some("batch-progress") => batch_progress(store, rest, call),
        Some("claim-verdicts") => claim_verdicts(store, rest, call),
        Some("set-repo") => set_repo(store, rest, call),
        Some("dep-add") => edge(store, "dep-add", rest, call),
        Some("dep-remove") => edge(store, "dep-remove", rest, call),
        Some("delete-bead-row") => delete_row(store, rest, call),
        other => (
            CANNOT_TELL,
            format!("{other:?} is not a control-store verb"),
        ),
    }
}

/// A machine step's or cascade's answer: the summary when it ran, the refusal when it did not.
pub(super) fn answered(
    ran: state_store::Result<Ran>,
    summary: impl FnOnce(&[bool]) -> Value,
) -> (i32, String) {
    match ran {
        Ok(Ran::Done { applied, .. }) => {
            let code = if applied.iter().all(|a| *a) {
                0
            } else {
                REFUSED
            };
            (code, summary(&applied).to_string())
        }
        Ok(Ran::Refused { refusal, .. }) => (REFUSED, format!("refused: {:?}", Some(refusal))),
        Ok(Ran::Turned(t)) => turned(t),
        Err(e) => cannot_tell(e),
    }
}

pub(super) fn turned(t: Turned) -> (i32, String) {
    match t {
        Turned::Refused(why) => (REFUSED, why),
        Turned::CannotTell(why) => (CANNOT_TELL, why),
    }
}

/// `id:tip,id:tip`; empty is no member.
pub(super) fn members(s: &str) -> Option<Vec<(String, String)>> {
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

/// `cut <batch-id> --repo R --head H --base B --members "id:tip,..." --actor A [--parent P]`.
fn cut<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(batch_id) = args.first() else {
        return (CANNOT_TELL, "cut: missing <batch-id>".into());
    };
    let (Some(repo), Some(head), Some(base), Some(members_s), Some(actor)) = (
        flag(args, "--repo"),
        flag(args, "--head"),
        flag(args, "--base"),
        flag(args, "--members"),
        flag(args, "--actor"),
    ) else {
        return (
            CANNOT_TELL,
            "cut: --repo, --head, --base, --members and --actor are all required".into(),
        );
    };
    let parent = flag(args, "--parent");
    let Some(members) = members(&members_s) else {
        return (
            CANNOT_TELL,
            "cut: --members must be \"id:tip,id:tip,...\"".into(),
        );
    };
    if members.is_empty() {
        return (
            CANNOT_TELL,
            "cut: --members must name at least one bead".into(),
        );
    }
    let spec = Cut {
        batch_id,
        repo: &repo,
        head: &head,
        base: &base,
        parent: parent.as_deref(),
        members: &members,
        actor: &actor,
        at: call.now,
    };
    let req = Request::new(call.request, &actor, "lc.cut");
    let ran = store
        .plan_cut(&spec)
        .and_then(|p| store.run_planned(p, &req, "cut"));
    let ids: Vec<&String> = members.iter().map(|(id, _)| id).collect();
    answered(
        ran,
        |applied| json!({ "batch_id": batch_id, "members": ids, "applied": applied }),
    )
}

/// `--expect S --version V --actor A`, as every batch verb takes them.
pub(super) fn batch_ask(
    verb: &str,
    args: &[String],
) -> Result<(BatchState, u64, String), (i32, String)> {
    let (Some(expect), Some(version), Some(actor)) = (
        flag(args, "--expect"),
        flag(args, "--version"),
        flag(args, "--actor"),
    ) else {
        return Err((
            CANNOT_TELL,
            format!("{verb}: --expect, --version and --actor are all required"),
        ));
    };
    let Ok(version) = version.parse::<u64>() else {
        return Err((
            CANNOT_TELL,
            format!("{verb}: --version must be a non-negative integer"),
        ));
    };
    let Some(expect) = BatchState::from_str(&expect) else {
        return Err((
            CANNOT_TELL,
            format!("{verb}: unknown batch state {expect:?}"),
        ));
    };
    Ok((expect, version, actor))
}

/// `land <batch-id> --expect S --version V --actor A --sha SHA`.
fn land<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(batch_id) = args.first() else {
        return (CANNOT_TELL, "land: missing <batch-id>".into());
    };
    let Some(sha) = flag(args, "--sha") else {
        return (
            CANNOT_TELL,
            "land: --expect, --version, --actor and --sha are all required".into(),
        );
    };
    let (expect, version, actor) = match batch_ask("land", args) {
        Ok(x) => x,
        Err(e) if e.1.contains("all required") => {
            return (
                CANNOT_TELL,
                "land: --expect, --version, --actor and --sha are all required".into(),
            )
        }
        Err(e) => return e,
    };
    let ev = BatchEvent {
        expect,
        version,
        kind: BatchEventKind::FastForward { sha },
        actor: actor.clone(),
    };
    let req = Request::new(call.request, &actor, "lc.land");
    let ran = store
        .plan_land(batch_id, &ev, call.now, &req)
        .and_then(|p| store.run_planned(p, &req, "land"));
    answered(
        ran,
        |applied| json!({ "batch_id": batch_id, "steps": applied.len(), "applied": applied }),
    )
}

/// `event batch|delivery <key> --expect S --version N --actor A --kind <json>`, once `event`
/// has checked the flags common to every machine.
#[allow(clippy::too_many_arguments)]
pub(super) fn event<B: Backend>(
    store: &Store<B>,
    machine: &str,
    key: &str,
    expect: &str,
    version: u64,
    actor: &str,
    kind_json: &str,
    call: &Call,
) -> (i32, String) {
    let req = Request::new(call.request, actor, "lc.event");
    let ran = match machine {
        "batch" => {
            let Some(expect) = BatchState::from_str(expect) else {
                return (
                    CANNOT_TELL,
                    format!("event: unknown batch state {expect:?}"),
                );
            };
            let kind: BatchEventKind = match serde_json::from_str(kind_json) {
                Ok(k) => k,
                Err(e) => {
                    return (
                        CANNOT_TELL,
                        format!("event: --kind did not parse as a batch event: {e}"),
                    )
                }
            };
            if let BatchEventKind::Failed { suite, n } = &kind {
                if let Err(e) = crate::mending::check_widths(key, *n, suite) {
                    return e;
                }
                match store.mending(key, *n, suite) {
                    Ok(Some(_)) => {
                        return (0, format!("already recorded: {suite} failed in pass {n}"))
                    }
                    Ok(None) => {}
                    Err(e) => return cannot_tell(e),
                }
            }
            let ev = BatchEvent {
                expect,
                version,
                kind,
                actor: actor.into(),
            };
            store.batch_event(key, &ev, call.now, &req)
        }
        _ => {
            let Some(expect) = DeliveryState::from_str(expect) else {
                return (
                    CANNOT_TELL,
                    format!("event: unknown delivery state {expect:?}"),
                );
            };
            let kind: DeliveryEventKind = match serde_json::from_str(kind_json) {
                Ok(k) => k,
                Err(e) => {
                    return (
                        CANNOT_TELL,
                        format!("event: --kind did not parse as a delivery event: {e}"),
                    )
                }
            };
            let ev = DeliveryEvent {
                expect,
                version,
                kind,
                actor: actor.into(),
            };
            store.delivery_event(key, &ev, call.now, &req)
        }
    };
    match ran {
        Ok(Ran::Done { applied, .. }) if applied.iter().all(|a| *a) => (0, String::new()),
        Ok(Ran::Done { .. }) => (REFUSED, "refused: lost the race to another writer".into()),
        other => answered(other, |_| Value::Null),
    }
}

/// A delivery as `show` renders it: a SQL row.
pub(super) fn delivery_view(d: &DeliveryRow) -> Value {
    json!({
        "bead_id": d.bead_id,
        "mode": d.mode.as_str(),
        "state": d.state.as_str(),
        "batch_id": d.batch_id,
        "pr": number(d.pr.map(|p| p as i64)),
        "merge_sha": d.merge_sha,
        "version": number(Some(d.version as i64)),
    })
}

/// `show-batch <batch-id>`: the round's row, or exit 1 with `{}`.
fn show_batch<B: Backend>(store: &Store<B>, args: &[String]) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "show-batch: missing <batch-id>".into());
    };
    let read = || -> state_store::Result<Option<Value>> {
        let Some((_, rec)) = store.batch(id)? else {
            return Ok(None);
        };
        let row = &rec.value.row;
        let progress = store.progress(&row.repo, id)?;
        Ok(Some(json!({
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
        })))
    };
    match read() {
        Ok(Some(v)) => (0, v.to_string()),
        Ok(None) => (1, "{}".into()),
        Err(e) => cannot_tell(e),
    }
}

/// `batches-of <bead-id>`: the unfinished rounds naming it, ready to feed `eject-member`.
fn batches_of<B: Backend>(store: &Store<B>, args: &[String]) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "batches-of: missing <bead-id>".into());
    };
    match store.batches_of(id) {
        Ok(rows) => {
            let out: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "batch_id": r.batch_id,
                        "state": r.state.as_str(),
                        "version": r.version.to_string(),
                    })
                })
                .collect();
            (0, Value::Array(out).to_string())
        }
        Err(e) => cannot_tell(e),
    }
}

/// `batch-progress <batch-id> --json <object>`: the running pass's counts beside the round. A
/// round that does not exist takes none, as the dolt path's update of no row answers.
fn batch_progress<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "batch-progress: missing <batch-id>".into());
    };
    let Some(body) = flag(args, "--json") else {
        return (CANNOT_TELL, "batch-progress: the payload is missing".into());
    };
    let progress = match serde_json::from_str::<Value>(&body) {
        Ok(v) if v.is_object() => v,
        _ => {
            return (
                CANNOT_TELL,
                "batch-progress: stdin must be one JSON object".into(),
            )
        }
    };
    let req = Request::new(call.request, "batch-progress", "lc.batch-progress");
    let set = || -> state_store::Result<()> {
        if let Some((_, rec)) = store.batch(id)? {
            store.set_progress(&rec.value.row.repo, id, progress, &req)?;
        }
        Ok(())
    };
    match set() {
        Ok(()) => (0, String::new()),
        Err(e) => cannot_tell(e),
    }
}

/// `claim-verdicts --json <payload>`: one dispatcher pass onto each record's readiness.
fn claim_verdicts<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(body) = flag(args, "--json") else {
        return (CANNOT_TELL, "claim-verdicts: the payload is missing".into());
    };
    let payload = match crate::claim_verdicts::checked(&body) {
        Ok(p) => p,
        Err(why) => return (REFUSED, why),
    };
    if payload.entries.is_empty() {
        return (0, String::new());
    }
    // An id named twice keeps its last entry, as the dolt path's last update wins; a txn may
    // not write one key twice.
    let last = |i: usize, id: &str| !payload.entries[i + 1..].iter().any(|e| e.id == id);
    let verdicts: Vec<Verdict> = payload
        .entries
        .iter()
        .enumerate()
        .filter(|(i, e)| last(*i, &e.id))
        .map(|(_, e)| Verdict {
            id: e.id.clone(),
            verdict: e.verdict.clone(),
            blocker: e.blocker.clone(),
            runes: e.runes.clone(),
        })
        .collect();
    let req = Request::new(call.request, "spira-claim", "lc.claim-verdicts");
    match store.publish_verdicts(&verdicts, &payload.computed_at, &req) {
        Ok(_) => (0, String::new()),
        Err(e) => cannot_tell(e),
    }
}

/// `set-repo <bead-id> <repo>|--clear [--only-if <repo>]`.
fn set_repo<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let (Some(id), Some(target)) = (args.first(), args.get(1)) else {
        return (
            CANNOT_TELL,
            "set-repo: usage: set-repo <bead-id> <repo>|--clear [--only-if <repo>]".into(),
        );
    };
    if !crate::cutover::is_row_key(id) {
        return (CANNOT_TELL, format!("set-repo: {id:?} is not a bead id"));
    }
    let only_if = flag(args, "--only-if");
    let repo = (target != "--clear").then_some(target.as_str());
    let req = Request::new(call.request, "set-repo", "lc.set-repo");
    match store.set_repo(id, repo, only_if.as_deref(), &req) {
        Ok(RepoSet::NoRecord) => (0, format!("set-repo: {id} has no lifecycle row")),
        Ok(RepoSet::NotNamed) => (
            0,
            format!(
                "set-repo: {id} does not name {}; unchanged",
                only_if.unwrap_or_default()
            ),
        ),
        Ok(RepoSet::Working) => (1, crate::ops::working_refusal(id)),
        Ok(RepoSet::Set) => (0, format!("set-repo: {id} -> {}", repo.unwrap_or("(none)"))),
        Err(e) => cannot_tell(e),
    }
}

/// `dep-add <id> <depends-on-id> [--type T]` and `dep-remove <id> <depends-on-id>`: the
/// prerequisite enters or leaves the record's readiness. Only a `blocks` edge blocks, so an
/// edge re-added under another type no longer does.
fn edge<B: Backend>(store: &Store<B>, verb: &str, args: &[String], call: &Call) -> (i32, String) {
    let is_key = crate::cutover::is_row_key;
    let (id, dep) = match (args.first(), args.get(1)) {
        (Some(id), Some(dep)) if is_key(id) && is_key(dep) => (id, dep),
        _ => {
            return (
                CANNOT_TELL,
                format!("{verb}: takes <bead-id> <depends-on-id>"),
            )
        }
    };
    let blocks = verb == "dep-add" && flag(args, "--type").is_none_or(|t| t == "blocks");
    let edge = if blocks { Edge::Blocks } else { Edge::Unblocks };
    let req = Request::new(call.request, verb, &format!("lc.{verb}"));
    match store.set_edge(id, dep, edge, &req) {
        Ok(_) => (0, String::new()),
        Err(e) => cannot_tell(e),
    }
}

/// `delete-bead-row <bead-id>`: `drop-row` has already judged the bead a kind with no
/// lifecycle, so the record goes now rather than waiting for the planning plane to close it.
fn delete_row<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(id) = args.first().filter(|id| crate::cutover::is_row_key(id)) else {
        return (CANNOT_TELL, "delete-bead-row: takes <bead-id>".into());
    };
    let req = Request::new(call.request, "drop-row", "lc.delete-bead-row");
    match store.delete(id, false, &req) {
        Ok(_) => (0, String::new()),
        Err(e) => cannot_tell(e),
    }
}

/// How many rounds `list --batches` shows when no scope is given, newest first.
const BATCHES_SHOWN: usize = 8;

/// `list --batches [--batch ID | --window SECS]`: the rounds with their members, ejections,
/// passes and last event, as the ops pane reads them. A member's title is the planning plane's,
/// joined by the planning reader (`plan::with_planning_fields`).
pub(super) fn list_batches<B: Backend>(
    store: &Store<B>,
    args: &[String],
    call: &Call,
) -> (i32, String) {
    let only = flag(args, "--batch");
    let window = flag(args, "--window").and_then(|w| w.parse::<i64>().ok());
    let read = || -> state_store::Result<Vec<Value>> {
        let mut rounds: Vec<_> = store
            .batches()?
            .into_iter()
            .map(|(_, r)| r.value)
            .filter(|r| match (&only, window) {
                (Some(id), _) => &r.row.batch_id == id,
                (None, Some(w)) => r.opened_at >= call.now - w || !r.row.state.is_terminal(),
                (None, None) => true,
            })
            .collect();
        rounds.sort_by(|a, b| (b.opened_at, &b.row.batch_id).cmp(&(a.opened_at, &a.row.batch_id)));
        if only.is_none() && window.is_none() {
            rounds.truncate(BATCHES_SHOWN);
        }
        let mut out = Vec::new();
        for r in rounds {
            let row = &r.row;
            let id = row.batch_id.as_str();
            let events: Vec<_> = store
                .history(id, Some("batch"))?
                .into_iter()
                .filter(|c| c.audit.applied)
                .collect();
            let folded: Vec<Value> = events
                .iter()
                .filter(|c| crate::passes::FETCHED.contains(&format!("'{}'", c.audit.event)))
                .map(|c| json!({ "event": c.audit.event, "evidence": c.audit.evidence, "at": c.audit.at }))
                .collect();
            let (passes, since) = crate::passes::fold(&folded);
            let mut members: Vec<Value> = r
                .members
                .iter()
                .map(|m| {
                    json!({ "batch_id": id, "bead_id": m.bead_id, "outcome": null, "tip": m.tip })
                })
                .collect();
            members.sort_by(|a, b| a["bead_id"].as_str().cmp(&b["bead_id"].as_str()));
            let ejected: Vec<Value> = events
                .iter()
                .filter(|c| c.audit.event == "Eject")
                .map(|c| {
                    let e = &c.audit.evidence["Eject"];
                    json!({ "batch_id": id, "bead_id": e["bead_id"], "reason": e["reason"] })
                })
                .collect();
            let last_at = events.iter().map(|c| c.audit.at).max();
            out.push(json!({
                "batch_id": id,
                "repo": row.repo,
                "state": row.state.as_str(),
                "parent": row.parent,
                "pr": null,
                "reason": row.reason,
                "pass": row.pass.to_string(),
                "phase": row.phase.map(|p| p.as_str()),
                "progress": store.progress(&row.repo, id)?,
                "opened_at": r.opened_at.to_string(),
                "version": row.version.to_string(),
                "head": row.head,
                "base": row.base,
                "run": row.run,
                "members": members,
                "ejected": ejected,
                // The dolt path reads opened_at as text, so its fallback never applies.
                "phase_since": since,
                "last_pass": passes.last().cloned(),
                "passes": passes,
                "last_at": number(last_at),
            }));
        }
        Ok(out)
    };
    match read() {
        Ok(rows) => (
            0,
            serde_json::to_string_pretty(&Value::Array(rows)).unwrap_or_default(),
        ),
        Err(e) => cannot_tell(e),
    }
}

/// `list --delivery [--state S]`: every delivery with when it entered its state.
pub(super) fn list_delivery<B: Backend>(store: &Store<B>, args: &[String]) -> (i32, String) {
    let state = flag(args, "--state");
    let read = || -> state_store::Result<Vec<Value>> {
        let mut out = Vec::new();
        for d in store.deliveries()? {
            let d = d.value;
            if state.as_deref().is_some_and(|s| s != d.state.as_str()) {
                continue;
            }
            let entered = store
                .history(&d.bead_id, Some("delivery"))?
                .into_iter()
                .filter(|c| c.audit.applied && c.audit.to_state == d.state.as_str())
                .map(|c| c.audit.at)
                .max();
            let mut v = delivery_view(&d);
            if let Value::Object(o) = &mut v {
                o.insert("entered_at".into(), number(entered));
            }
            out.push(v);
        }
        Ok(out)
    };
    match read() {
        Ok(rows) => (
            0,
            serde_json::to_string_pretty(&Value::Array(rows)).unwrap_or_default(),
        ),
        Err(e) => cannot_tell(e),
    }
}

/// A blocking prerequisite still blocks while its own record is live; one landed, retired or
/// never recorded blocks nothing, as the dolt path's join on live rows reads it.
pub(super) fn still_blocking(state: Option<lifecycle::bead::BeadState>) -> bool {
    state.is_some_and(|s| !s.is_terminal())
}
