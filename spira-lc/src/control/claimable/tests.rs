use std::collections::{BTreeMap, BTreeSet};

use lifecycle::bead::{BeadState, HoldKind, Stack};

use super::*;
use crate::control::records::ClaimInputs;

fn rec(id: &str, state: BeadState) -> LifecycleRecord {
    let mut r = LifecycleRecord::born(id, Some("spira"), None, 100);
    r.row.state = state;
    r.row.tip = Some(format!("tip-{id}"));
    r.claim = Some(ClaimInputs::default());
    r
}

fn blocked_on(id: &str, state: BeadState, blocker: &str) -> LifecycleRecord {
    let mut r = rec(id, state);
    r.claim.as_mut().unwrap().blocks.push(blocker.into());
    r
}

fn rows(recs: &[&LifecycleRecord]) -> BTreeMap<String, LifecycleRecord> {
    recs.iter().map(|r| (r.row.bead_id.clone(), (*r).clone())).collect()
}

fn ctx() -> Ctx {
    Ctx { now: 1_000, stack_max: 4, incident_label: "incident".into(), pilgrim_label: "pilgrimage".into(), skuld_label: "draft".into(), ..Ctx::default() }
}

fn verdict(cand: &LifecycleRecord, others: &[&LifecycleRecord], ctx: &Ctx) -> Judgement {
    let mut all = others.to_vec();
    all.push(cand);
    judge(cand, &rows(&all), &BTreeMap::new(), ctx)
}

fn blocked(of: &str) -> (Verdict, Option<String>) {
    (Verdict::Blocked, Some(of.into()))
}

fn seen(j: &Judgement) -> (Verdict, Option<String>) {
    (j.verdict, j.blocker.clone())
}

#[test_id(tpsmwlw)]
#[test]
fn a_member_of_an_open_integration_is_carried_and_the_integration_is_not() {
    let int = rec("I", BeadState::Ready);
    let mut member = rec("A", BeadState::Ready);
    member.carried_by = Some("I".into());
    assert_eq!(seen(&verdict(&member, &[&int], &ctx())), (Verdict::Carried, Some("I".into())));
    assert_eq!(verdict(&int, &[], &ctx()).verdict, Verdict::Claimable);
}

#[test_id(t7x1b43)]
#[test]
fn a_member_carried_by_a_landed_integration_is_claimable_and_one_carried_by_no_record_is_carried() {
    let landed = rec("I", BeadState::Landed);
    let mut member = rec("A", BeadState::Ready);
    member.carried_by = Some("I".into());
    assert_eq!(verdict(&member, &[&landed], &ctx()).verdict, Verdict::Claimable);
    assert_eq!(verdict(&member, &[], &ctx()).verdict, Verdict::Carried);
}

#[test_id(tkre5ds)]
#[test]
fn an_incident_never_stacks_on_a_certified_fix_but_claims_once_it_lands() {
    let certified = rec("A", BeadState::Certified);
    let mut inc = blocked_on("B", BeadState::Ready, "A");
    inc.claim.as_mut().unwrap().labels.push("incident".into());
    assert_eq!(seen(&verdict(&inc, &[&certified], &ctx())), blocked("A"));
    let plain = blocked_on("B", BeadState::Ready, "A");
    let j = verdict(&plain, &[&certified], &ctx());
    assert_eq!((j.verdict, j.depth), (Verdict::Claimable, 1), "a non-incident still stacks");
    let landed = rec("A", BeadState::Landed);
    let j = verdict(&inc, &[&landed], &ctx());
    assert_eq!((j.verdict, j.depth), (Verdict::Claimable, 0));
}

#[test_id(tbgaxm3)]
#[test]
fn a_pilgrimage_never_stacks_on_an_in_delivery_feature_but_claims_once_it_lands() {
    let mut pilgrim = blocked_on("B", BeadState::Ready, "A");
    pilgrim.claim.as_mut().unwrap().labels.push("pilgrimage".into());
    for open in [BeadState::InDelivery, BeadState::Certified] {
        assert_eq!(seen(&verdict(&pilgrim, &[&rec("A", open)], &ctx())), blocked("A"));
    }
    let plain = blocked_on("B", BeadState::Ready, "A");
    let j = verdict(&plain, &[&rec("A", BeadState::InDelivery)], &ctx());
    assert_eq!((j.verdict, j.depth), (Verdict::Claimable, 1), "an unlabelled bead still stacks");
    let j = verdict(&pilgrim, &[&rec("A", BeadState::Landed)], &ctx());
    assert_eq!((j.verdict, j.depth), (Verdict::Claimable, 0));
}

#[test_id(t8xdlai)]
#[test]
fn a_skuld_request_waits_for_its_blocker_to_land_and_never_stacks_on_it() {
    let mut skuld = blocked_on("B", BeadState::Ready, "A");
    skuld.claim.as_mut().unwrap().labels.push("draft".into());
    for open in [BeadState::InDelivery, BeadState::Certified] {
        assert_eq!(seen(&verdict(&skuld, &[&rec("A", open)], &ctx())), blocked("A"));
    }
    let plain = blocked_on("B", BeadState::Ready, "A");
    let j = verdict(&plain, &[&rec("A", BeadState::InDelivery)], &ctx());
    assert_eq!((j.verdict, j.depth), (Verdict::Claimable, 1), "an unlabelled bead still stacks");
    let j = verdict(&skuld, &[&rec("A", BeadState::Landed)], &ctx());
    assert_eq!((j.verdict, j.depth), (Verdict::Claimable, 0));
}

#[test_id(tttvcvt)]
#[test]
fn stacked_blocker_certified_is_claimable() {
    let j = verdict(&blocked_on("B", BeadState::Ready, "A"), &[&rec("A", BeadState::Certified)], &ctx());
    assert_eq!((j.verdict, j.depth), (Verdict::Claimable, 1));
}

#[test_id(tqlwxev)]
#[test]
fn a_rework_on_the_stack_the_gate_went_red_on_is_not_claimable_until_the_tip_moves() {
    let mut a = rec("A", BeadState::Certified);
    let mut b = blocked_on("B", BeadState::Rework, "A");
    b.row.stack = Stack::from([("A".to_string(), "tip-A".to_string())]);
    b.row.reason = Some("suites-failed".into());
    assert_eq!(verdict(&b, &[&a], &ctx()).verdict, Verdict::StackUnchanged);
    a.row.tip = Some("moved".into());
    assert_eq!(verdict(&b, &[&a], &ctx()).verdict, Verdict::Claimable);
    a.row.tip = Some("tip-A".into());
    b.row.reason = Some("base_withdrawn: A x".into());
    assert_eq!(verdict(&b, &[&a], &ctx()).verdict, Verdict::Claimable, "only a gate-red reason repeats");
}

#[test_id(tqmmqpy)]
#[test]
fn stack_plan_names_the_stacked_prerequisites_tip() {
    let j = verdict(&blocked_on("B", BeadState::Ready, "A"), &[&rec("A", BeadState::Certified)], &ctx());
    assert_eq!(j.depth, 1);
    assert_eq!(j.stack.get("A").map(String::as_str), Some("tip-A"));
}

#[test_id(t82x51w)]
#[test]
fn stack_plan_omits_a_landed_blocker_it_does_not_stack_on() {
    let j = verdict(&blocked_on("B", BeadState::Ready, "A"), &[&rec("A", BeadState::Landed)], &ctx());
    assert_eq!(j.depth, 0);
    assert!(j.stack.is_empty(), "a landed prerequisite is satisfied, not stacked on");
}

#[test_id(thkjzfs)]
#[test]
fn stack_plan_too_deep_still_reports_the_attempted_stack() {
    let mut a = rec("A", BeadState::InDelivery);
    a.row.stack_depth = 4;
    let j = verdict(&blocked_on("B", BeadState::Ready, "A"), &[&a], &ctx());
    assert_eq!((j.verdict, j.depth), (Verdict::TooDeep, 5));
    assert_eq!(j.stack.get("A").map(String::as_str), Some("tip-A"));
}

fn conflict(id: &str, tip: &str) -> Ctx {
    Ctx { conflicts: BTreeSet::from([(id.to_string(), tip.to_string())]), ..ctx() }
}

#[test_id(tz3fnjb)]
#[test]
fn a_dependent_of_a_conflicting_parent_is_blocked_until_the_parent_rebases() {
    let mut a = rec("A", BeadState::Certified);
    let b = blocked_on("B", BeadState::Ready, "A");
    assert_eq!(verdict(&b, &[&a], &ctx()).verdict, Verdict::Claimable, "no marker: stacked as before");
    assert_eq!(seen(&verdict(&b, &[&a], &conflict("A", "tip-A"))), blocked("A"));
    a.row.tip = Some("tip-A-rebased".into());
    assert_eq!(verdict(&b, &[&a], &conflict("A", "tip-A")).verdict, Verdict::Claimable, "the parent's tip moved");
}

#[test_id(t0s9op6)]
#[test]
fn an_old_marker_on_an_in_delivery_parent_does_not_block_its_dependent() {
    let a = rec("A", BeadState::InDelivery);
    let b = blocked_on("B", BeadState::Rework, "A");
    assert_eq!(seen(&verdict(&b, &[&a], &conflict("A", "tip-A"))), blocked("A"), "a held marker blocks");
    assert_eq!(verdict(&b, &[&a], &ctx()).verdict, Verdict::Claimable, "a lapsed marker is not in the context");
}

#[test_id(tnlwvpe)]
#[test]
fn a_dependent_waiting_on_a_conflicting_blocker_is_released_when_the_blocker_lands() {
    let mut a = rec("A", BeadState::Certified);
    let b = blocked_on("B", BeadState::Rework, "A");
    assert_eq!(seen(&verdict(&b, &[&a], &conflict("A", "tip-A"))), blocked("A"));
    a.row.state = BeadState::Landed;
    let j = verdict(&b, &[&a], &conflict("A", "tip-A"));
    assert_eq!((j.verdict, j.depth), (Verdict::Claimable, 0), "landed: one rebase, no stack");
}

#[test_id(t2tu70b)]
#[test]
fn stacked_blocker_only_submitted_waits() {
    let j = verdict(&blocked_on("B", BeadState::Ready, "A"), &[&rec("A", BeadState::Submitted)], &ctx());
    assert_eq!(seen(&j), blocked("A"));
}

#[test_id(t8e179m)]
#[test]
fn a_blocker_landing_releases_its_dependent_with_no_hand_step_but_a_manual_hold_never_lifts() {
    let dep = blocked_on("B", BeadState::Ready, "A");
    let no_stack = Ctx { stack_max: 0, ..ctx() };
    for blocker in [BeadState::Working, BeadState::Submitted] {
        assert_eq!(seen(&verdict(&dep, &[&rec("A", blocker)], &no_stack)), blocked("A"), "{blocker:?}");
    }
    let landed = rec("A", BeadState::Landed);
    assert_eq!(verdict(&dep, &[&landed], &no_stack).verdict, Verdict::Claimable);
    let mut parked = dep.clone();
    parked.row.holds.insert(HoldKind::Manual);
    assert_eq!(verdict(&parked, &[&landed], &no_stack).verdict, Verdict::Held);
}

#[test_id(tdc1fbi)]
#[test]
fn a_record_whose_claim_inputs_were_never_written_cannot_be_told_and_is_never_claimable() {
    let mut r = rec("A", BeadState::Ready);
    r.claim = None;
    let j = verdict(&r, &[], &ctx());
    assert_eq!((j.verdict, j.missing), (Verdict::CannotTell, vec!["claim"]));
    let mut r = rec("A", BeadState::Ready);
    r.repo = None;
    let j = verdict(&r, &[], &ctx());
    assert_eq!((j.verdict, j.missing), (Verdict::CannotTell, vec!["repo"]));
}

#[test_id(t8jlexa)]
#[test]
fn a_held_record_with_no_claim_inputs_is_held_not_cannot_tell() {
    let mut r = rec("A", BeadState::Ready);
    r.claim = None;
    r.row.holds.insert(HoldKind::Manual);
    assert_eq!(verdict(&r, &[], &ctx()).verdict, Verdict::Held);
}

#[test_id(txbcuce)]
#[test]
fn a_wait_hold_with_a_snooze_in_the_future_is_snoozed_and_a_lapsed_one_is_not() {
    let mut r = rec("A", BeadState::Ready);
    r.row.holds.insert(HoldKind::Wait);
    r.row.reason = Some(spira_store::lc_state::snooze_reason(2_000));
    assert_eq!(verdict(&r, &[], &ctx()).verdict, Verdict::Snoozed);
    r.row.reason = Some(spira_store::lc_state::snooze_reason(500));
    assert_eq!(verdict(&r, &[], &ctx()).verdict, Verdict::Claimable);
}

#[test_id(tsdqa3v)]
#[test]
fn a_no_loop_labelled_record_is_out_of_scope_and_a_missing_scope_label_is_too() {
    let mut r = rec("A", BeadState::Ready);
    r.claim.as_mut().unwrap().labels.push("fixture-parked".into());
    let c = Ctx { no_loop_label: "fixture-parked".into(), ..ctx() };
    assert_eq!(verdict(&r, &[], &c).verdict, Verdict::OutOfScope);
    let plain = rec("A", BeadState::Ready);
    let c = Ctx { scope_label: "mine".into(), ..ctx() };
    assert_eq!(verdict(&plain, &[], &c).verdict, Verdict::OutOfScope);
}

#[test_id(ttdcz21)]
#[test]
fn a_reserved_id_or_a_stopped_repo_is_stopped_but_a_blocked_record_there_stays_blocked() {
    let r = rec("A", BeadState::Ready);
    let reserved = Ctx { reserved: BTreeSet::from(["A".to_string()]), ..ctx() };
    assert_eq!(verdict(&r, &[], &reserved).verdict, Verdict::Stopped);
    let stopped = Ctx { stopped_repos: BTreeSet::from(["spira".to_string()]), ..ctx() };
    assert_eq!(verdict(&r, &[], &stopped).verdict, Verdict::Stopped);
    let b = blocked_on("B", BeadState::Ready, "A");
    assert_eq!(seen(&verdict(&b, &[&rec("A", BeadState::Working)], &stopped)), blocked("A"));
}

#[test_id(tl5n71n)]
#[test]
fn a_rowless_gate_blocks_while_open_and_releases_when_closed() {
    let b = blocked_on("B", BeadState::Ready, "G");
    let judged = |status: &str| {
        let rowless = BTreeMap::from([("G".to_string(), ("gate".to_string(), status.to_string()))]);
        judge(&b, &rows(&[&b]), &rowless, &ctx())
    };
    let open = judged("open");
    assert_eq!(seen(&open), blocked("G"));
    assert_eq!(open.gates.get("G"), Some(&None));
    assert_eq!(judged("closed").verdict, Verdict::Claimable);
}

#[test_id(t5porwg)]
#[test]
fn gates_name_every_blocker_read_with_its_gate() {
    let b = blocked_on("B", BeadState::Ready, "A");
    let a = rec("A", BeadState::Working);
    let j = verdict(&b, &[&a], &ctx());
    assert_eq!(j.gates, BTreeMap::from([("A".to_string(), Some(a.gate()))]));
}

mod pinned {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use state_store::{Config as StoreConfig, Contract, MemBackend, Ports};

    use super::*;

    struct Counting<'a> {
        configs: &'a AtomicUsize,
        parts: &'a AtomicUsize,
        fail_config: &'a Cell<bool>,
        home: Option<&'a Path>,
        fail_parts: &'a Cell<bool>,
    }

    impl Inputs for Counting<'_> {
        fn config(&self) -> Result<Config, String> {
            self.configs.fetch_add(1, Ordering::SeqCst);
            if self.fail_config.get() {
                return Err("unreadable".into());
            }
            FIXED.config()
        }
        fn statuses(&self, ids: &[String]) -> Result<BTreeMap<String, (String, String)>, String> {
            FIXED.statuses(ids)
        }
        fn items(&self, ids: &[String]) -> Result<Vec<serde_json::Value>, String> {
            FIXED.items(ids)
        }
        fn run_dir(&self) -> Result<PathBuf, String> {
            FIXED.run_dir()
        }
        fn partitions(&self, config: &Config) -> Result<Vec<Partition>, String> {
            self.parts.fetch_add(1, Ordering::SeqCst);
            if self.fail_parts.get() {
                return Err("runes unreadable".into());
            }
            match self.home {
                Some(h) => runes::partitions(h, Some("r"), &config.queue_wait, ""),
                None => Ok(Vec::new()),
            }
        }
        fn partitions_stamp(&self) -> Result<PartitionsStamp, String> {
            Ok(self.home.map(|h| runes::partitions_stamp(h, Some("r"))).unwrap_or_default())
        }
    }

    pub(super) fn store() -> Store<MemBackend> {
        let clock = runtime::ports::VirtualClock::new(1_000_000);
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), StoreConfig { min_lock_ttl_ms: 1_000 }).unwrap();
        Store::new(c, 1_000)
    }

    fn call(s: &Store<MemBackend>, inputs: &dyn Inputs) -> Result<ViewRead, String> {
        let mut cand = rec("A", BeadState::Ready);
        cand.claim.as_mut().unwrap().labels.push("spira".into());
        view(s, 1, vec![cand], inputs, 1_000)
    }

    #[test_id(tso2e9r)]
    #[test]
    fn fifty_views_read_the_config_and_the_partitions_once() {
        let nofail = Cell::new(false);
        let (configs, parts, fail) = (AtomicUsize::new(0), AtomicUsize::new(0), Cell::new(false));
        let pinned = Pinned::new(Counting { configs: &configs, parts: &parts, fail_config: &fail, home: None, fail_parts: &nofail });
        let s = store();
        for _ in 0..50 {
            call(&s, &pinned).unwrap();
        }
        assert_eq!((configs.load(Ordering::SeqCst), parts.load(Ordering::SeqCst)), (1, 1));
        let bare = Counting { configs: &configs, parts: &parts, fail_config: &fail, home: None, fail_parts: &nofail };
        for _ in 0..50 {
            call(&s, &bare).unwrap();
        }
        assert_eq!((configs.load(Ordering::SeqCst), parts.load(Ordering::SeqCst)), (51, 51), "unpinned inputs read every call");
    }

    #[test_id(ti746b8)]
    #[test]
    fn a_rune_edit_between_calls_takes_effect_on_the_next() {
        let home = testkit::TempDir::new("claimable-pinned-rune-edit");
        let dir = spira_config::runes::runes_dir(&home);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("r.rune"), "RUNE_LABELS=spira\n").unwrap();
        let nofail = Cell::new(false);
        let (configs, parts, fail) = (AtomicUsize::new(0), AtomicUsize::new(0), Cell::new(false));
        let pinned = Pinned::new(Counting { configs: &configs, parts: &parts, fail_config: &fail, home: Some(&home), fail_parts: &nofail });
        let s = store();
        let runes_of = |r: ViewRead| r.1["A"].runes.clone();
        assert_eq!(runes_of(call(&s, &pinned).unwrap()), vec!["r".to_string()]);
        assert_eq!(runes_of(call(&s, &pinned).unwrap()), vec!["r".to_string()]);
        assert_eq!(parts.load(Ordering::SeqCst), 1);
        std::fs::write(dir.join("r.rune"), "RUNE_LABELS=other-label-entirely\n").unwrap();
        assert!(runes_of(call(&s, &pinned).unwrap()).is_empty());
        assert_eq!(parts.load(Ordering::SeqCst), 2);
    }

    #[test_id(tfrghd1)]
    #[test]
    fn a_failed_config_read_is_not_kept() {
        let nofail = Cell::new(false);
        let (configs, parts, fail) = (AtomicUsize::new(0), AtomicUsize::new(0), Cell::new(true));
        let pinned = Pinned::new(Counting { configs: &configs, parts: &parts, fail_config: &fail, home: None, fail_parts: &nofail });
        let s = store();
        assert!(call(&s, &pinned).is_err());
        fail.set(false);
        call(&s, &pinned).unwrap();
        assert_eq!(configs.load(Ordering::SeqCst), 2);
    }

    #[test_id(tbnmmli)]
    #[test]
    fn a_failed_partitions_read_is_not_kept() {
        let (configs, parts, fail, fail_parts) = (AtomicUsize::new(0), AtomicUsize::new(0), Cell::new(false), Cell::new(true));
        let pinned = Pinned::new(Counting { configs: &configs, parts: &parts, fail_config: &fail, home: None, fail_parts: &fail_parts });
        let s = store();
        let _ = call(&s, &pinned);
        fail_parts.set(false);
        call(&s, &pinned).unwrap();
        call(&s, &pinned).unwrap();
        assert_eq!(parts.load(Ordering::SeqCst), 2, "the failure was retried, then the success kept");
    }
}

fn delivered(id: &str, reason: Option<&str>) -> LifecycleRecord {
    let mut r = rec(id, BeadState::InDelivery);
    r.row.reason = reason.map(str::to_string);
    r
}

#[test_id(t9vkk8d)]
#[test]
fn a_blocker_delivered_from_submitted_blocks_the_dependent_until_it_lands() {
    let dependent = blocked_on("B", BeadState::Ready, "A");
    let provisional = delivered("A", Some("delivered-from-submitted"));
    assert_eq!(seen(&verdict(&dependent, &[&provisional], &ctx())), blocked("A"));
    let mut landed = provisional.clone();
    landed.row.state = BeadState::Landed;
    assert_eq!(verdict(&dependent, &[&landed], &ctx()).verdict, Verdict::Claimable);
}

#[test_id(tyq5psz)]
#[test]
fn a_blocker_delivered_from_certified_still_carries_a_stack() {
    let dependent = blocked_on("B", BeadState::Ready, "A");
    let j = verdict(&dependent, &[&delivered("A", None)], &ctx());
    assert_eq!((j.verdict, j.depth), (Verdict::Claimable, 1));
}

mod shipped_runes {
    use state_store::MemBackend;

    use super::*;

    struct Discovered<'a>(&'a Path);

    impl Inputs for Discovered<'_> {
        fn config(&self) -> Result<Config, String> {
            FIXED.config()
        }
        fn statuses(&self, ids: &[String]) -> Result<BTreeMap<String, (String, String)>, String> {
            FIXED.statuses(ids)
        }
        fn items(&self, ids: &[String]) -> Result<Vec<serde_json::Value>, String> {
            FIXED.items(ids)
        }
        fn run_dir(&self) -> Result<PathBuf, String> {
            FIXED.run_dir()
        }
        fn partitions(&self, config: &Config) -> Result<Vec<Partition>, String> {
            runes::partitions(self.0, None, &config.queue_wait, "")
        }
        fn partitions_stamp(&self) -> Result<PartitionsStamp, String> {
            Ok(runes::partitions_stamp(self.0, None))
        }
    }

    fn label_names(text: &str) -> BTreeSet<String> {
        text.lines()
            .map(str::trim_start)
            .filter(|l| l.starts_with("RUNE_LABELS=") || l.starts_with("RUNE_EXCLUDE_LABELS="))
            .flat_map(|l| l.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map(str::to_string).collect::<Vec<_>>())
            .filter(|w| w.starts_with("SPIRA_") && w.ends_with("_LABEL"))
            .collect()
    }

    // covers: UC-dispatch-09
    #[test_id(tbmx1k4)]
    #[test]
    fn the_shipped_spike_partition_takes_its_own_bead_and_no_other_kind() {
        let ws = testkit::TempDir::new("spira-lc-shipped-spike-partition");
        let dir = runes::runes_dir(&ws);
        std::fs::create_dir_all(&dir).unwrap();
        let mut names = BTreeSet::new();
        for entry in std::fs::read_dir(testkit::manifest_dir().join("../spira/runes")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "rune") {
                let text = std::fs::read_to_string(&path).unwrap();
                names.extend(label_names(&text));
                std::fs::write(dir.join(path.file_name().unwrap()), text).unwrap();
            }
        }
        let pin = |n: &str| if n == "SPIRA_SPIKE_LABEL" { "research".to_string() } else { format!("pinned-{n}") };
        let pins: Vec<(&str, Option<String>)> = names.iter().map(|n| (n.as_str(), Some(pin(n)))).collect();
        let pairs: Vec<(&str, Option<&str>)> = pins.iter().map(|(k, v)| (*k, v.as_deref())).collect();
        let _env = testkit::env(&pairs);
        let label = |n: &str| pin(n);
        let (scope, spike, plan) = (label("SPIRA_SCOPE_LABEL"), label("SPIRA_SPIKE_LABEL"), label("SPIRA_PLAN_LABEL"));

        let cand = |id: &str, labels: &[&str], hold: Option<HoldKind>| {
            let mut r = rec(id, BeadState::Ready);
            r.claim.as_mut().unwrap().labels = labels.iter().map(|l| l.to_string()).collect();
            r.row.holds.extend(hold);
            r
        };
        let cands = vec![
            cand("spike", &[&scope, &spike], None),
            cand("plan", &[&scope, &plan], None),
            cand("partial", &[&spike], None),
            cand("asked", &[&scope, &spike], Some(HoldKind::Ask)),
            cand("poisoned", &[&scope, &spike], Some(HoldKind::Poison)),
        ];

        let parts = runes::partitions(&ws, None, &FIXED.config().unwrap().queue_wait, "").unwrap();
        assert!(parts.iter().any(|p| p.name == "spike"), "the spike rune is discovered without being listed");

        let s = super::pinned::store();
        let (_, views) = view::<MemBackend>(&s, 1, cands, &Discovered(&ws), 1_000).unwrap();
        let seen: Vec<(&str, Verdict, Vec<String>)> = ["spike", "plan", "partial", "asked", "poisoned"]
            .into_iter()
            .map(|id| (id, views[id].verdict, views[id].runes.clone()))
            .collect();
        let want = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            seen,
            vec![
                ("spike", Verdict::Claimable, want(&["spike"])),
                ("plan", Verdict::Claimable, want(&["builder"])),
                ("partial", Verdict::Claimable, want(&[])),
                ("asked", Verdict::Held, want(&[])),
                ("poisoned", Verdict::Held, want(&[])),
            ]
        );
    }
}
