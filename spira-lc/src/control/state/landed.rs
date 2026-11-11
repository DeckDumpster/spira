//! `reconcile-landed <bead> --sha SHA --ref REF (--repo-path P | --repo NAME) --actor A` on the
//! control store: work that verifiably merged while the lifecycle missed it. The proof is
//! ancestry, SHA reachable from REF, and either a message on SHA that names the bead or SHA
//! being the row's own recorded tip (a row whose tip is itself a landing commit names another
//! bead, and its tip on the ref is the stronger proof). A delivery still BATCHED, PR_OPEN or
//! PUSHING exits Delivered with SHA.

use lifecycle::bead::{BeadEvent, BeadEventKind};
use lifecycle::delivery::{DeliveryEvent, DeliveryEventKind, DeliveryState};
use serde_json::json;
use state_store::{Backend, Request};

use super::{answer, cannot_tell, Call};
use crate::control::store::Store;
use crate::{flag, CANNOT_TELL, REFUSED};

pub const VERBS: &[&str] = &["reconcile-landed"];

/// What git said about the claimed landing.
pub struct Landing<'a> {
    pub sha: &'a str,
    pub base: &'a str,
    pub on_ref: bool,
    pub names_bead: bool,
}

pub fn run<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "reconcile-landed: missing <bead-id>".into());
    };
    let (Some(sha), Some(base), Some(actor)) = (flag(args, "--sha"), flag(args, "--ref"), flag(args, "--actor")) else {
        return (CANNOT_TELL, "reconcile-landed: --sha, --ref and --actor are required".into());
    };
    let repo = match (flag(args, "--repo-path"), flag(args, "--repo")) {
        (Some(p), _) => std::path::PathBuf::from(p),
        (None, Some(name)) => match crate::repo_config::from_process().map(|c| c.repos.get(&name).map(|s| s.path.clone())) {
            Ok(Some(p)) => std::path::PathBuf::from(p),
            Ok(None) => return (CANNOT_TELL, format!("reconcile-landed: no repository {name:?} in the config in force")),
            Err(e) => return (CANNOT_TELL, format!("reconcile-landed: {e}")),
        },
        (None, None) => return (CANNOT_TELL, "reconcile-landed: --repo-path or --repo is required".into()),
    };
    let on_ref = crate::git_evidence::is_ancestor(&repo, &sha, &base);
    let names_bead = on_ref && crate::git_evidence::commit_names_bead(&repo, &sha, id);
    settle(store, id, &Landing { sha: &sha, base: &base, on_ref, names_bead }, &actor, call)
}

pub fn settle<B: Backend>(store: &Store<B>, id: &str, landing: &Landing, actor: &str, call: &Call) -> (i32, String) {
    let Landing { sha, base, on_ref, names_bead } = *landing;
    if !on_ref {
        return (REFUSED, format!("refused: {sha} is not reachable from {base}"));
    }
    let rec = match store.record(id) {
        Ok(Some(r)) => r.value,
        Ok(None) => return (REFUSED, format!("refused: no bead row for {id}")),
        Err(e) => return cannot_tell(e),
    };
    if !names_bead && rec.row.tip.as_deref() != Some(sha) {
        return (REFUSED, format!("refused: the commit message of {sha} does not name {id}, and {sha} is not its recorded tip"));
    }
    let proof = format!("reconcile-landed:{sha} ancestor-of:{base}");
    let ev = BeadEvent {
        expect: rec.row.state,
        version: rec.row.version,
        kind: BeadEventKind::ContentOnBase { proof: proof.clone() },
        actor: actor.to_string(),
        at: Some(call.now),
    };
    let req = Request::new(call.request, actor, "lc.reconcile-landed");
    let (code, out) = answer(store.apply(id, &ev, &req));
    if code != 0 {
        return (code, out);
    }
    let mut delivered = false;
    match store.delivery(id) {
        Ok(Some(d)) if matches!(d.value.state, DeliveryState::Batched | DeliveryState::PrOpen | DeliveryState::Pushing) => {
            let dev = DeliveryEvent {
                expect: d.value.state,
                version: d.value.version,
                kind: DeliveryEventKind::Delivered { merge_sha: sha.to_string(), proof },
                actor: actor.to_string(),
            };
            let req = Request::new(call.request, actor, "lc.reconcile-landed.delivery");
            delivered = matches!(store.delivery_event(id, &dev, call.now, &req), Ok(crate::control::store::Ran::Done { .. }));
        }
        Ok(_) => {}
        Err(e) => return cannot_tell(e),
    }
    (0, json!({"bead_id": id, "sha": sha, "delivered": delivered}).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports};

    const TIP: &str = "a6cae2a78a6cae2a78a6cae2a78a6cae2a78a6ca";
    const OTHER: &str = "3a767c1bb3a767c1bb3a767c1bb3a767c1bb3a76";

    fn store() -> Store<MemBackend> {
        let clock = VirtualClock::new(100_000);
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        Store::new(c, 1_000)
    }

    fn lc(s: &Store<MemBackend>, argv: &[&str]) -> (i32, String) {
        let args: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
        let request = format!("t/{argv:?}");
        super::super::run(s, &args, &Call { now: 100, request: &request })
    }

    fn state_of(s: &Store<MemBackend>, id: &str) -> String {
        let (_, out) = lc(s, &["show", id]);
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["bead"]["state"].as_str().unwrap().to_string()
    }

    fn submitted(s: &Store<MemBackend>, id: &str, tip: &str) {
        assert_eq!(lc(s, &["create-bead", id, "--repo", "spira", "--submitted-tip", tip]).0, 0);
    }

    fn certified(s: &Store<MemBackend>, id: &str, tip: &str) {
        submitted(s, id, tip);
        let kind = json!({"GatePass": {"tip": tip, "gate_key": "g"}}).to_string();
        let (code, out) = lc(s, &["event", "bead", id, "--expect", "SUBMITTED", "--version", "1", "--actor", "t", "--kind", &kind]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(state_of(s, id), "CERTIFIED");
    }

    fn settle_with(s: &Store<MemBackend>, id: &str, sha: &str, on_ref: bool, names_bead: bool) -> (i32, String) {
        let landing = Landing { sha, base: "refs/heads/local/main", on_ref, names_bead };
        settle(s, id, &landing, "concierge", &Call { now: 100, request: &format!("t/settle/{id}") })
    }

    #[test]
    fn sp_lrnzs9_a_certified_bead_whose_tip_is_on_the_landing_ref_settles_to_landed() {
        let s = store();
        certified(&s, "sp-a", TIP);
        let (code, out) = settle_with(&s, "sp-a", TIP, true, true);
        assert_eq!(code, 0, "{out}");
        assert_eq!(state_of(&s, "sp-a"), "LANDED");
    }

    #[test]
    fn sp_lrnzs9_a_tip_not_on_the_landing_ref_is_refused_naming_both_shas() {
        let s = store();
        certified(&s, "sp-a", TIP);
        let (code, out) = settle_with(&s, "sp-a", TIP, false, true);
        assert_eq!(code, REFUSED, "{out}");
        assert!(out.contains(TIP) && out.contains("refs/heads/local/main"), "{out}");
        assert_eq!(state_of(&s, "sp-a"), "CERTIFIED");
    }

    #[test]
    fn sp_lrnzs9_a_submitted_row_whose_recorded_tip_is_another_beads_landing_commit_settles() {
        let s = store();
        submitted(&s, "sp-b", OTHER);
        let (code, out) = settle_with(&s, "sp-b", OTHER, true, false);
        assert_eq!(code, 0, "{out}");
        assert_eq!(state_of(&s, "sp-b"), "LANDED");
    }

    #[test]
    fn sp_lrnzs9_a_commit_that_neither_names_the_bead_nor_is_its_tip_is_refused() {
        let s = store();
        submitted(&s, "sp-b", TIP);
        let (code, out) = settle_with(&s, "sp-b", OTHER, true, false);
        assert_eq!(code, REFUSED, "{out}");
        assert_eq!(state_of(&s, "sp-b"), "SUBMITTED");
    }

    #[test]
    fn sp_lrnzs9_a_row_the_store_does_not_hold_is_refused_not_invented() {
        let (code, out) = settle_with(&store(), "sp-none", TIP, true, true);
        assert_eq!(code, REFUSED, "{out}");
        assert!(out.contains("no bead row"), "{out}");
    }
}
