//! The round's verbs past the cut on the control store, each answering as its dolt path does:
//! `settle`, `abandon-batch`, `eject-member`, `stack`, `stage`, `promote` and
//! `requeue-orphans`.

use std::collections::BTreeMap;

use lifecycle::batch::{BatchEvent, BatchEventKind};
use serde_json::{json, Map};
use state_store::{Backend, Request};

use super::rounds::{answered, batch_ask, members, turned};
use super::{cannot_tell, Call};
use crate::control::store::{Cut, Ran, Store, Withdrawn};
use crate::{flag, CANNOT_TELL, REFUSED};

/// The verbs this module answers.
pub const VERBS: &[&str] = &[
    "settle",
    "abandon-batch",
    "eject-member",
    "stack",
    "stage",
    "promote",
    "requeue-orphans",
];

/// A round that opened and showed no sign of life for this long is a phantom.
const PHANTOM_BATCH_SECS: i64 = 120;

pub fn run<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let rest = &args[1..];
    match args.first().map(String::as_str) {
        Some("settle") => settle(store, rest, call),
        Some("abandon-batch") => abandon(store, rest, call),
        Some("eject-member") => eject(store, rest, call),
        Some("stack") => stack(store, rest, call),
        Some("stage") => stage(store, rest, call),
        Some("promote") => promote(store, rest, call),
        Some("requeue-orphans") => requeue_orphans(store, rest, call),
        other => (
            CANNOT_TELL,
            format!("{other:?} is not a control-store verb"),
        ),
    }
}

/// `a,b`; empty is none.
fn ids(s: Option<String>) -> Vec<String> {
    s.filter(|s| !s.is_empty())
        .map(|s| s.split(',').map(String::from).collect())
        .unwrap_or_default()
}

fn stacked_on(w: &Withdrawn) -> BTreeMap<&str, &str> {
    w.iter().map(|(d, p)| (d.as_str(), p.as_str())).collect()
}

/// `settle <batch-id> --expect S --version N --actor A [--eject id,...] [--requeue id,...]`.
fn settle<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "settle: missing <batch-id>".into());
    };
    let (expect, version, actor) = match batch_ask("settle", args) {
        Ok(x) => x,
        Err(e) => return e,
    };
    let eject = ids(flag(args, "--eject"));
    let requeue = ids(flag(args, "--requeue"));
    let ev = BatchEvent {
        expect,
        version,
        kind: BatchEventKind::Settle,
        actor: actor.clone(),
    };
    let req = Request::new(call.request, &actor, "lc.settle");
    let planned = store.plan_settle(id, &ev, &eject, &requeue, call.now, &req);
    let (ran, withdrawn) = match planned {
        Ok((p, w)) => (store.run_planned(p, &req, "settle"), w),
        Err(e) => return cannot_tell(e),
    };
    let mut depth = Map::new();
    if matches!(ran, Ok(Ran::Done { .. })) {
        for m in eject.iter().chain(withdrawn.iter().map(|(d, _)| d)) {
            match store.record(m) {
                Ok(Some(r)) => {
                    depth.insert(m.clone(), json!(r.value.row.stack_depth));
                }
                Ok(None) => {}
                Err(e) => return cannot_tell(e),
            }
        }
    }
    answered(ran, |applied| {
        json!({
            "batch_id": id,
            "steps": applied.len(),
            "applied": applied,
            "base_withdrawn": withdrawn.len(),
            "stacked_on": stacked_on(&withdrawn),
            "stack_depth": depth,
        })
    })
}

/// `abandon-batch <batch-id> --expect S --version N --actor A --reason R`.
fn abandon<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    const USAGE: &str = "abandon-batch: --expect, --version, --actor and --reason are all required";
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "abandon-batch: missing <batch-id>".into());
    };
    let Some(reason) = flag(args, "--reason") else {
        return (CANNOT_TELL, USAGE.into());
    };
    let (expect, version, actor) = match batch_ask("abandon-batch", args) {
        Ok(x) => x,
        Err(e) if e.1.contains("all required") => return (CANNOT_TELL, USAGE.into()),
        Err(e) => return e,
    };
    let ev = BatchEvent {
        expect,
        version,
        kind: BatchEventKind::Abandon { reason },
        actor: actor.clone(),
    };
    let req = Request::new(call.request, &actor, "lc.abandon-batch");
    let ran = store
        .plan_abandon(id, &ev, call.now, &req)
        .and_then(|p| store.run_planned(p, &req, "abandon"));
    answered(
        ran,
        |applied| json!({ "batch_id": id, "steps": applied.len(), "applied": applied }),
    )
}

/// `eject-member <batch-id> --bead-id ID --expect S --version N --actor A --reason R`.
fn eject<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    const USAGE: &str =
        "eject-member: --bead-id, --expect, --version, --actor and --reason are all required";
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "eject-member: missing <batch-id>".into());
    };
    let (Some(bead), Some(reason)) = (flag(args, "--bead-id"), flag(args, "--reason")) else {
        return (CANNOT_TELL, USAGE.into());
    };
    let (expect, version, actor) = match batch_ask("eject-member", args) {
        Ok(x) => x,
        Err(e) if e.1.contains("all required") => return (CANNOT_TELL, USAGE.into()),
        Err(e) => return e,
    };
    let ev = BatchEvent {
        expect,
        version,
        kind: BatchEventKind::Eject {
            bead_id: bead.clone(),
            reason,
        },
        actor: actor.clone(),
    };
    let req = Request::new(call.request, &actor, "lc.eject-member");
    let (ran, withdrawn) = match store.plan_eject(id, &bead, &ev, call.now, &req) {
        Ok((p, w)) => (store.run_planned(p, &req, "eject"), w),
        Err(e) => return cannot_tell(e),
    };
    answered(ran, |applied| {
        json!({
            "batch_id": id,
            "bead_id": bead,
            "steps": applied.len(),
            "applied": applied,
            "base_withdrawn": withdrawn.len(),
            "stacked_on": stacked_on(&withdrawn),
        })
    })
}

/// `--members "id:tip,..."`, naming at least one bead.
fn member_list(verb: &str, args: &[String]) -> Result<Vec<(String, String)>, (i32, String)> {
    let parsed = flag(args, "--members").map(|m| members(&m));
    match parsed {
        Some(Some(m)) if !m.is_empty() => Ok(m),
        Some(Some(_)) => Err((
            CANNOT_TELL,
            format!("{verb}: --members must name at least one bead"),
        )),
        _ => Err((
            CANNOT_TELL,
            format!("{verb}: --members must be \"id:tip,id:tip,...\""),
        )),
    }
}

/// `stack <batch-id> --members "id:tip,..." --actor A`.
fn stack<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "stack: missing <batch-id>".into());
    };
    let (Some(_), Some(actor)) = (flag(args, "--members"), flag(args, "--actor")) else {
        return (
            CANNOT_TELL,
            "stack: --members and --actor are both required".into(),
        );
    };
    let members = match member_list("stack", args) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let req = Request::new(call.request, &actor, "lc.stack");
    let ran = store
        .plan_stack(id, &members, &actor, call.now)
        .and_then(|p| store.run_planned(p, &req, "stack"));
    let ids: Vec<&String> = members.iter().map(|(m, _)| m).collect();
    answered(
        ran,
        |applied| json!({ "batch_id": id, "members": ids, "applied": applied }),
    )
}

/// `stage <batch-id> --repo R --head H --base B --members "id:tip,..." --actor A --parent P`.
fn stage<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "stage: missing <batch-id>".into());
    };
    let (Some(repo), Some(head), Some(base), Some(_), Some(actor), Some(parent)) = (
        flag(args, "--repo"),
        flag(args, "--head"),
        flag(args, "--base"),
        flag(args, "--members"),
        flag(args, "--actor"),
        flag(args, "--parent"),
    ) else {
        return (
            CANNOT_TELL,
            "stage: --repo, --head, --base, --members, --actor and --parent are all required"
                .into(),
        );
    };
    let members = match member_list("stage", args) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let spec = Cut {
        batch_id: id,
        repo: &repo,
        head: &head,
        base: &base,
        parent: Some(&parent),
        members: &members,
        actor: &actor,
        at: call.now,
    };
    let req = Request::new(call.request, &actor, "lc.stage");
    match store.stage(&spec, &req) {
        Ok(Ran::Done { applied, .. }) if applied.iter().all(|a| *a) => {
            let ids: Vec<&String> = members.iter().map(|(m, _)| m).collect();
            (0, json!({ "batch_id": id, "members": ids }).to_string())
        }
        Ok(Ran::Done { .. }) => (REFUSED, "refused: lost the race to another writer".into()),
        Ok(Ran::Refused { refusal, .. }) => (REFUSED, format!("refused: {:?}", Some(refusal))),
        Ok(Ran::Turned(t)) => turned(t),
        Err(e) => cannot_tell(e),
    }
}

/// `promote <batch-id> --head H --base B --actor A [--round <json>]`.
fn promote<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "promote: missing <batch-id>".into());
    };
    let (Some(head), Some(base), Some(actor)) = (
        flag(args, "--head"),
        flag(args, "--base"),
        flag(args, "--actor"),
    ) else {
        return (
            CANNOT_TELL,
            "promote: --head, --base and --actor are all required".into(),
        );
    };
    let round = match super::rounds::round_fields("promote", args) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let req = Request::new(call.request, &actor, "lc.promote");
    let staged = match store.batch(id) {
        Ok(b) => b.map(|(_, r)| r.value.members),
        Err(e) => return cannot_tell(e),
    };
    let ran = store
        .plan_promote(id, &head, &base, &actor, call.now, &req)
        .and_then(|p| p.with_round(id, round.as_ref()))
        .and_then(|p| store.run_planned(p, &req, "promote"));
    let ids: Vec<String> = staged
        .unwrap_or_default()
        .into_iter()
        .map(|m| m.bead_id)
        .collect();
    answered(
        ran,
        |applied| json!({ "batch_id": id, "members": ids, "applied": applied }),
    )
}

/// `requeue-orphans --actor A [--apply] [--stale-secs N] [--live-round BATCH]...`.
fn requeue_orphans<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(actor) = flag(args, "--actor") else {
        return (CANNOT_TELL, "requeue-orphans: --actor is required".into());
    };
    let apply = args.iter().any(|a| a == "--apply");
    let stale_secs = match flag(args, "--stale-secs").map(|s| s.parse::<i64>()) {
        None => PHANTOM_BATCH_SECS,
        Some(Ok(n)) if n >= 0 => n,
        Some(_) => {
            return (
                CANNOT_TELL,
                "requeue-orphans: --stale-secs must be a non-negative integer".into(),
            )
        }
    };
    let live: std::collections::BTreeSet<String> = args
        .windows(2)
        .filter(|w| w[0] == "--live-round")
        .map(|w| w[1].clone())
        .collect();
    let req = Request::new(call.request, &actor, "lc.requeue-orphans");
    match store.requeue_orphans(&actor, apply, stale_secs, call.now, &live, &req) {
        Ok(o) => {
            if apply {
                if let Some(said) = log_abandons(sweep_run_dir(), &o.abandoned, &o.repos, stale_secs, call.now) {
                    eprintln!("{said}");
                }
            }
            let out = json!({
                "orphans": o.orphans,
                "phantom_batches": o.phantom_batches,
                "abandoned": o.abandoned,
                "requeued": o.requeued,
                "failed": o.failed,
                "leased": o.leased,
                "running": o.running,
                "repos": o.repos,
            });
            let code = if o.failed.is_empty() { 0 } else { REFUSED };
            (code, out.to_string())
        }
        Err(e) => cannot_tell(e),
    }
}

/// Tests of the handler never write to an ambient run directory.
fn sweep_run_dir() -> Result<std::path::PathBuf, String> {
    if cfg!(test) {
        return Err("no run directory under test".into());
    }
    crate::lifecycle_run_dir()
}

/// One `ROUND-ABANDON` line in `landing.log` for each round the sweep abandoned, in the shape
/// the queue writes for its own abandons; the failure to write is returned for the caller to say.
fn log_abandons(
    run: Result<std::path::PathBuf, String>,
    abandoned: &[String],
    repos: &BTreeMap<String, String>,
    stale_secs: i64,
    now: i64,
) -> Option<String> {
    use std::io::Write;
    if abandoned.is_empty() {
        return None;
    }
    let file = run.and_then(|dir| {
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("landing.log")).map_err(|e| e.to_string())
    });
    match file {
        Ok(mut f) => {
            for id in abandoned {
                let repo = repos.get(id).map_or("unknown", String::as_str);
                let _ = writeln!(f, "QUEUE ROUND-ABANDON {now} repo={repo} batch={id} reason=requeue-orphans: opened and never began, no driver in {stale_secs}s");
            }
            None
        }
        Err(e) => Some(format!("spira-lc: {e} — landing.log not written for {}", abandoned.join(", "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abandoned() -> (Vec<String>, BTreeMap<String, String>) {
        (vec!["b-1".into()], [("b-1".to_string(), "spira".to_string()), ("b-2".to_string(), "other".to_string())].into())
    }

    #[test]
    fn sp_pi2ojm_a_round_the_sweep_abandons_is_logged_as_a_round_abandon() {
        let dir = testkit::TempDir::new("lc-sweep-abandon-log");
        let (ids, repos) = abandoned();
        assert_eq!(log_abandons(Ok(dir.path().to_path_buf()), &ids, &repos, 120, 1_794_368_406), None);
        let log = std::fs::read_to_string(dir.path().join("landing.log")).unwrap();
        assert_eq!(log.lines().count(), 1, "{log}");
        assert!(log.starts_with("QUEUE ROUND-ABANDON 1794368406 repo=spira batch=b-1 reason=requeue-orphans: "), "{log}");
        assert!(log.contains("120s"), "{log}");
    }

    #[test]
    fn sp_pi2ojm_a_sweep_that_abandons_nothing_writes_no_log() {
        let dir = testkit::TempDir::new("lc-sweep-abandon-none");
        assert_eq!(log_abandons(Ok(dir.path().to_path_buf()), &[], &abandoned().1, 120, 1), None);
        assert!(!dir.path().join("landing.log").exists());
    }

    #[test]
    fn sp_pi2ojm_an_unreadable_run_dir_is_said_not_swallowed() {
        let (ids, repos) = abandoned();
        let said = log_abandons(Err("no run".into()), &ids, &repos, 120, 1);
        assert!(said.is_some_and(|s| s.contains("no run") && s.contains("landing.log")));
    }
}
