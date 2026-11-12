//! The readiness and sweep verbs on the control store: `publish-readiness`, the dispatcher's
//! pass for every live record; `expire-claims`, which returns each record whose claim lock is
//! gone to the state it was claimed from; and `sweep-open`, which promotes a record a crashed
//! filing left OPEN, or names it once for escalation when no open bead stands behind it.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::json;
use spira_config::edges::Gate;
use state_store::{Backend, Request};

use super::state::Call;
use super::store::{Filing, Judged, Pass, Store};
use crate::{flag, CANNOT_TELL, REFUSED};

/// The verbs this module answers.
pub const VERBS: &[&str] = &["publish-readiness", "expire-claims", "sweep-open", "mark-rejudge"];

pub fn run<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let rest = &args[1..];
    let answer = match args.first().map(String::as_str) {
        Some("publish-readiness") => publish(store, rest, call),
        Some("expire-claims") => expire(store, rest, call),
        Some("sweep-open") => sweep_open(store, rest, call),
        Some("mark-rejudge") => mark_rejudge(store, rest, call),
        other => {
            return (
                CANNOT_TELL,
                format!("{other:?} is not a control-store verb"),
            )
        }
    };
    match answer {
        Ok(out) => (0, out),
        Err((code, e)) => (code, e),
    }
}

type Answer = Result<String, (i32, String)>;

fn cannot_tell(e: impl std::fmt::Display) -> (i32, String) {
    (CANNOT_TELL, format!("cannot tell: {e}"))
}

fn ids(args: &[String], name: &str) -> Vec<String> {
    flag(args, name)
        .map(|s| {
            s.split(',')
                .filter(|x| !x.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct Payload {
    computed_at: String,
    #[serde(default)]
    planning_watermark: Option<String>,
    #[serde(default)]
    judged_at_rev: Option<u64>,
    entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    #[serde(default)]
    claimable: bool,
    #[serde(default)]
    blocked_by: Vec<String>,
    #[serde(default)]
    gates: BTreeMap<String, Option<Gate>>,
    #[serde(default)]
    verdict: Option<String>,
    #[serde(default)]
    blocker: Option<String>,
    #[serde(default)]
    runes: Vec<String>,
}

/// `publish-readiness --json <payload>`: `{computed_at, planning_watermark?, judged_at_rev?,
/// entries: [{id, claimable, blocked_by, gates, verdict?, blocker?, runes?}]}`, one entry per
/// live record the pass judged, whatever its state.
fn publish<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> Answer {
    let body = flag(args, "--json").ok_or_else(|| {
        (
            CANNOT_TELL,
            "publish-readiness: the payload is missing".into(),
        )
    })?;
    let p: Payload =
        serde_json::from_str(&body).map_err(|e| (REFUSED, format!("publish-readiness: {e}")))?;
    let judged: Vec<Judged> = p
        .entries
        .into_iter()
        .map(|e| Judged {
            id: e.id,
            claimable: e.claimable,
            blocked_by: e.blocked_by,
            gates: e.gates,
            verdict: e.verdict,
            blocker: e.blocker,
            runes: e.runes,
        })
        .collect();
    let pass = Pass {
        judged_at: p.computed_at,
        planning_watermark: p.planning_watermark,
        judged_at_rev: p.judged_at_rev,
    };
    let req = Request::new(call.request, "spira-claim", "lc.publish-readiness");
    let rev = store
        .publish_readiness(&judged, &pass, &req)
        .map_err(cannot_tell)?;
    Ok(json!({ "rev": rev }).to_string())
}

/// `mark-rejudge <id> [--blocked-by a,b]`: a planning write changed what the bead's claim reads.
fn mark_rejudge<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> Answer {
    let id = args.first().filter(|a| !a.starts_with('-')).ok_or_else(|| (CANNOT_TELL, "mark-rejudge: the bead id is missing".into()))?;
    let req = Request::new(call.request, "spira-lc", "lc.mark-rejudge");
    let rev = store.mark_rejudge(id, &ids(args, "--blocked-by"), &req).map_err(cannot_tell)?;
    Ok(json!({ "rev": rev }).to_string())
}

/// `expire-claims --stopped a,b --actor A`: each named record still WORKING with no live claim
/// lock goes back to the state it was claimed from. The caller names only the records whose
/// holder it has stopped.
fn expire<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> Answer {
    let stopped = ids(args, "--stopped");
    let actor = flag(args, "--actor").unwrap_or_else(|| "sentinel".into());
    let req = Request::new(call.request, &actor, "lc.expire-claims");
    let returned = store
        .expire_claims(|r| stopped.contains(&r.row.bead_id), &actor, call.now, &req)
        .map_err(cannot_tell)?;
    Ok(json!({ "returned": returned }).to_string())
}

/// `sweep-open --grace-secs N --open a,b --gone c,d [--escalated c] --actor A`. `--open` and
/// `--gone` are the planning plane's answer for the OPEN records it was asked about. Prints the
/// records promoted, those marked escalated (the caller's `--escalated`), and those still to
/// escalate; a record marked once is never named again.
fn sweep_open<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> Answer {
    let grace = flag(args, "--grace-secs")
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|g| *g >= 0)
        .ok_or_else(|| {
            (
                CANNOT_TELL,
                "sweep-open: --grace-secs takes a whole number of seconds".into(),
            )
        })?;
    let mut filing = BTreeMap::new();
    filing.extend(ids(args, "--open").into_iter().map(|i| (i, Filing::Open)));
    filing.extend(ids(args, "--gone").into_iter().map(|i| (i, Filing::Gone)));
    let escalated = ids(args, "--escalated");
    let actor = flag(args, "--actor").unwrap_or_else(|| "sentinel".into());
    let req = Request::new(call.request, &actor, "lc.sweep-open");
    let mut escalate = Vec::new();
    let out = store
        .sweep_open(
            grace,
            call.now,
            &filing,
            |id| {
                let done = escalated.iter().any(|e| e == id);
                if !done {
                    escalate.push(id.to_string());
                }
                done
            },
            &actor,
            &req,
        )
        .map_err(cannot_tell)?;
    Ok(json!({
        "promoted": out.promoted,
        "escalated": out.escalated,
        "escalate": escalate,
    })
    .to_string())
}

#[cfg(test)]
mod tests;
