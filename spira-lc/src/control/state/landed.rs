//! `reconcile-landed <bead> --sha SHA --ref REF (--repo-path P | --repo NAME) --actor A` on the
//! control store: work that verifiably merged while the lifecycle missed it. The CLI reads git
//! and sends the result as `--on-ref` and `--names-bead`; the service runs no git. The proof is
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

/// The caller's half: git is asked here, in the CLI's process, and the answer rides to the
/// service as `--on-ref` and `--names-bead`, which `run` trusts as `deliver` trusts its proof.
pub fn with_proof(args: &[String]) -> Result<Vec<String>, (i32, String)> {
    let refuse = |why: String| (CANNOT_TELL, format!("reconcile-landed: {why}"));
    let Some(id) = args.first() else { return Err(refuse("missing <bead-id>".into())) };
    let (Some(sha), Some(base)) = (flag(args, "--sha"), flag(args, "--ref")) else {
        return Err(refuse("--sha, --ref and --actor are required".into()));
    };
    let repo = match (flag(args, "--repo-path"), flag(args, "--repo")) {
        (Some(p), _) => std::path::PathBuf::from(p),
        (None, Some(name)) => crate::repo_config::registered_root(&name).map_err(|e| refuse(e.to_string()))?,
        (None, None) => return Err(refuse("--repo-path or --repo is required".into())),
    };
    let on_ref = crate::git_evidence::is_ancestor(&repo, &sha, &base);
    let names_bead = on_ref && crate::git_evidence::commit_names_bead(&repo, &sha, id);
    let mut proved = args.to_vec();
    proved.extend(["--on-ref".into(), on_ref.to_string(), "--names-bead".into(), names_bead.to_string()]);
    Ok(proved)
}

pub fn run<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "reconcile-landed: missing <bead-id>".into());
    };
    let (Some(sha), Some(base), Some(actor)) = (flag(args, "--sha"), flag(args, "--ref"), flag(args, "--actor")) else {
        return (CANNOT_TELL, "reconcile-landed: --sha, --ref and --actor are required".into());
    };
    let (Some(on_ref), Some(names_bead)) = (flag(args, "--on-ref"), flag(args, "--names-bead")) else {
        return (CANNOT_TELL, "reconcile-landed: --on-ref and --names-bead are required: the caller proves the landing from git".into());
    };
    settle(store, id, &Landing { sha: &sha, base: &base, on_ref: on_ref == "true", names_bead: names_bead == "true" }, &actor, call)
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
        lc_args(s, &argv.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    fn lc_args(s: &Store<MemBackend>, args: &[String]) -> (i32, String) {
        let request = format!("t/{args:?}");
        super::super::run(s, args, &Call { now: 100, request: &request, inputs: &crate::control::claimable::FIXED })
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
        settle(s, id, &landing, "concierge", &Call { now: 100, request: &format!("t/settle/{id}"), inputs: &crate::control::claimable::FIXED })
    }

    #[test_id(tkjugrk)]
    #[test]
    fn sp_lrnzs9_a_certified_bead_whose_tip_is_on_the_landing_ref_settles_to_landed() {
        let s = store();
        certified(&s, "sp-a", TIP);
        let (code, out) = settle_with(&s, "sp-a", TIP, true, true);
        assert_eq!(code, 0, "{out}");
        assert_eq!(state_of(&s, "sp-a"), "LANDED");
    }

    #[test_id(t7g7czb)]
    #[test]
    fn sp_lrnzs9_a_tip_not_on_the_landing_ref_is_refused_naming_both_shas() {
        let s = store();
        certified(&s, "sp-a", TIP);
        let (code, out) = settle_with(&s, "sp-a", TIP, false, true);
        assert_eq!(code, REFUSED, "{out}");
        assert!(out.contains(TIP) && out.contains("refs/heads/local/main"), "{out}");
        assert_eq!(state_of(&s, "sp-a"), "CERTIFIED");
    }

    #[test_id(tfaru8e)]
    #[test]
    fn sp_lrnzs9_a_submitted_row_whose_recorded_tip_is_another_beads_landing_commit_settles() {
        let s = store();
        submitted(&s, "sp-b", OTHER);
        let (code, out) = settle_with(&s, "sp-b", OTHER, true, false);
        assert_eq!(code, 0, "{out}");
        assert_eq!(state_of(&s, "sp-b"), "LANDED");
    }

    #[test_id(t4cq1pb)]
    #[test]
    fn sp_lrnzs9_a_commit_that_neither_names_the_bead_nor_is_its_tip_is_refused() {
        let s = store();
        submitted(&s, "sp-b", TIP);
        let (code, out) = settle_with(&s, "sp-b", OTHER, true, false);
        assert_eq!(code, REFUSED, "{out}");
        assert_eq!(state_of(&s, "sp-b"), "SUBMITTED");
    }

    #[test_id(tashh6c)]
    #[test]
    fn sp_lrnzs9_a_row_the_store_does_not_hold_is_refused_not_invented() {
        let (code, out) = settle_with(&store(), "sp-none", TIP, true, true);
        assert_eq!(code, REFUSED, "{out}");
        assert!(out.contains("no bead row"), "{out}");
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test_id(texkudd)]
    #[test]
    fn sp_lrnzs9_the_verb_asks_git_itself_so_a_tip_on_the_ref_settles_and_one_off_it_does_not() {
        let dir = testkit::TempDir::new("lc-reconcile-landed");
        let repo = dir.path();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["commit", "-q", "--allow-empty", "-m", "sp-on: landed work"]);
        let on = git(repo, &["rev-parse", "HEAD"]);
        git(repo, &["checkout", "-q", "-b", "side"]);
        git(repo, &["commit", "-q", "--allow-empty", "-m", "sp-off: unlanded work"]);
        let off = git(repo, &["rev-parse", "HEAD"]);
        let s = store();
        submitted(&s, "sp-on", &on);
        submitted(&s, "sp-off", &off);
        let verb = |id: &str, sha: &str| {
            lc_args(&s, &proved(&["reconcile-landed", id, "--sha", sha, "--ref", "main", "--repo-path", &repo.to_string_lossy(), "--actor", "concierge"]).unwrap())
        };
        let recorder = std::sync::Arc::new(state_store::phases::Recorder::default());
        let (code, out) = verb("sp-off", &off);
        assert_eq!(code, REFUSED, "{out}");
        assert!(out.contains(&off) && out.contains("main"), "{out}");
        assert_eq!(state_of(&s, "sp-off"), "SUBMITTED");
        let args = proved(&["reconcile-landed", "sp-on", "--sha", &on, "--ref", "main", "--repo-path", &repo.to_string_lossy(), "--actor", "concierge"]).unwrap();
        state_store::phases::track(recorder.clone());
        let (code, out) = lc_args(&s, &args);
        state_store::phases::untrack();
        assert_eq!(code, 0, "{out}");
        assert_eq!(recorder.snapshot().children, 0, "the served verb ran no git");
        assert_eq!(state_of(&s, "sp-on"), "LANDED");
    }

    #[test_id(tm9fjuk)]
    #[test]
    fn the_service_refuses_a_request_the_caller_did_not_prove() {
        let s = store();
        submitted(&s, "sp-a", TIP);
        let (code, out) = lc(&s, &["reconcile-landed", "sp-a", "--sha", TIP, "--ref", "main", "--repo-path", "nowhere", "--actor", "concierge"]);
        assert_eq!(code, CANNOT_TELL, "{out}");
        assert_eq!(state_of(&s, "sp-a"), "SUBMITTED");
    }

    fn proved(argv: &[&str]) -> Result<Vec<String>, (i32, String)> {
        with_proof(&argv.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_product_repository_known_only_to_the_repo_map_is_reconciled_by_name() {
        let dir = testkit::TempDir::new("lc-reconcile-landed-registry");
        let repo = dir.path().join("dumpstar");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "sp-on: landed work"]);
        let on = git(&repo, &["rev-parse", "HEAD"]);
        let run = dir.path().join("run");
        let env = spira_config::process::fixture_world_env(dir.path(), &run, &["spira", "dumpstar"]);
        let edits: Vec<(&str, Option<&str>)> = env.iter().map(|(k, v)| (*k, v.as_deref())).collect();
        let _env = testkit::env(&edits);
        let s = store();
        submitted(&s, "sp-on", &on);
        let verb = |name: &str| match proved(&["reconcile-landed", "sp-on", "--sha", &on, "--ref", "main", "--repo", name, "--actor", "concierge"]) {
            Ok(args) => lc_args(&s, &args),
            Err(refused) => refused,
        };
        let (code, out) = verb("dumpstar");
        assert_eq!(code, 0, "{out}");
        assert_eq!(state_of(&s, "sp-on"), "LANDED");
        let (code, out) = verb("nowhere");
        assert_eq!(code, CANNOT_TELL, "{out}");
        assert!(out.contains("no repository \"nowhere\""), "{out}");
        let (code, out) = verb("");
        assert_eq!(code, CANNOT_TELL, "{out}");
        let (code, out) = verb("spira");
        assert_eq!(code, REFUSED, "the home repo resolves to a checkout that lacks the commit: {out}");
        assert!(out.contains("not reachable"), "{out}");
    }
}
