use std::sync::Mutex;

use serde_json::json;
use state_store::Request;

use super::*;
use crate::outcome::Kind;
use crate::ports::Lc;
use crate::real::Unserved;
use crate::sim::bench::{Bench, EPOCH_MS};

const MIN: u64 = 60_000;

fn lane(b: &Bench) {
    b.seed(
        "lanes/lane",
        json!({ "landing_ref": "base-0", "image_digest": "sha256:img-0", "harness_version": "h0" }),
    );
}

fn round(b: &Bench, batch: &str, state: &str, tested_base: &str, extra: Value) {
    let mut v = json!({
        "batch_id": batch, "repo": "lane", "state": state, "head": format!("{batch}-h1"), "base": tested_base,
        "pass": 1, "version": 1, "tested_base": tested_base, "members": [{ "id": format!("sp-{batch}"), "tip": "t1" }],
    });
    if let (Some(o), Value::Object(x)) = (v.as_object_mut(), extra) {
        o.extend(x);
    }
    b.seed(&format!("batches/lane/{batch}"), v);
}

fn lease(b: &Bench, resource: &str) {
    b.store
        .lock(
            resource,
            3_600_000,
            &Request::new(&format!("lock-{resource}"), "t", "t"),
        )
        .unwrap();
}

fn held_stacked(b: &Bench, batch: &str, parent: &str, seen_ms: u64) {
    b.seed(
        &land::status_key("lane", batch),
        json!({ "blocker": "stacked-on-unlanded", "parent": parent, "parent_state": "ATTRIBUTING", "parent_leased": false,
                "inputs": { "head": format!("{batch}-h1"), "base": "base-0", "parent": parent, "parent_state": null, "parent_head": null },
                "green_seen_ms": seen_ms }),
    );
}

fn built(b: &Bench) -> Built {
    build(&b.ports(), "lane")
        .unwrap()
        .expect("the lane has a record")
}

fn level_of(b: &Built, f: &str) -> f64 {
    b.fields[f]["value"].as_f64().unwrap()
}

fn reason(b: &Built, t: &str) -> Option<String> {
    b.conditions
        .iter()
        .find(|c| c.type_ == t)
        .unwrap()
        .raised
        .clone()
}

/// A stuck parent with a GREEN child stacked on it, the child held by the Lander ten minutes ago.
fn stuck(b: &Bench) {
    lane(b);
    round(
        b,
        "p1",
        "Attributing",
        "base-0",
        json!({ "last_lease_token": { "main": 7, "sub": 0 } }),
    );
    round(b, "c1", "Green", "p1-h1", json!({ "parent": "p1" }));
    held_stacked(b, "c1", "p1", EPOCH_MS - 10 * MIN);
}

#[test]
fn a_repo_with_no_lane_record_has_no_section() {
    let b = Bench::new();
    assert_eq!(build(&b.ports(), "lane").unwrap(), None);
    assert_eq!(publish(&b.ports(), "lane").unwrap(), Publish::NoLane);
    assert!(b.published().is_empty());
}

#[test]
fn a_green_child_of_an_unleased_attributing_parent_raises_stacked_on_undriven_parent() {
    let b = Bench::new();
    stuck(&b);
    let s = built(&b);
    let green = s.fields["green_unlanded"].as_array().unwrap();
    assert_eq!(green.len(), 1);
    assert_eq!(green[0]["batch"], "c1");
    assert_eq!(green[0]["members"], json!(["sp-c1@t1"]));
    assert_eq!(green[0]["age_minutes"], 10);
    assert_eq!(green[0]["blocker"]["blocker"], "stacked-on-unlanded");
    assert_eq!(
        green[0]["undriven_parent"],
        "parent p1 ATTRIBUTING, unleased"
    );
    assert_eq!(s.green_unlanded_minutes, 10);
    assert_eq!(
        reason(&s, STACKED_ON_UNDRIVEN_PARENT).as_deref(),
        Some("c1: parent p1 ATTRIBUTING, unleased")
    );
    assert_eq!(
        level_of(&s, "orphans_not_redriven"),
        1.0,
        "the parent is an orphan too"
    );
    assert_eq!(s.fields["rounds"], json!({ "ATTRIBUTING": 1, "GREEN": 1 }));
    assert_eq!(level_of(&s, "stack_depth"), 2.0);

    let sec = section("lane", &s, None, EPOCH_MS);
    let kind =
        spira_desired_state::section::admit_publish(&sec).expect("the built section is admitted");
    let d = sec.distance_value(kind).unwrap().unwrap();
    assert!((d - (10.0 / 5.0 + 1.0)).abs() < 1e-9, "{d}");
}

#[test]
fn a_leased_parent_or_a_green_one_is_driven_and_raises_nothing() {
    let b = Bench::new();
    stuck(&b);
    lease(&b, "round/lane/p1");
    let s = built(&b);
    assert_eq!(reason(&s, STACKED_ON_UNDRIVEN_PARENT), None);
    assert_eq!(
        s.fields["leased"],
        json!([{ "batch": "p1", "state": "ATTRIBUTING", "phase": null }])
    );
    assert_eq!(level_of(&s, "orphans_not_redriven"), 0.0);

    let b = Bench::new();
    lane(&b);
    round(&b, "p1", "Green", "base-0", json!({}));
    round(&b, "c1", "Green", "p1-h1", json!({ "parent": "p1" }));
    let s = built(&b);
    assert_eq!(
        reason(&s, STACKED_ON_UNDRIVEN_PARENT),
        None,
        "a GREEN parent is the Lander's"
    );
    assert_eq!(
        s.green_unlanded_minutes, 0,
        "a round the Lander has not held yet waited no time"
    );
    assert!(s.fields["green_unlanded"][0].get("blocker").is_none());
}

#[test]
fn rounds_never_leased_wait_for_a_slot_and_saturate_only_when_every_slot_is_taken() {
    let mut b = Bench::new();
    b.slot_settings.spill = false;
    lane(&b);
    round(&b, "o1", "Open", "base-0", json!({}));
    let s = built(&b);
    assert_eq!(level_of(&s, "waiting_with_no_slot"), 1.0);
    assert_eq!(reason(&s, SLOTS_SATURATED), None, "the one slot is free");
    lease(&b, "runner-slot/proxmox/0");
    let s = built(&b);
    assert_eq!(
        reason(&s, SLOTS_SATURATED).as_deref(),
        Some("1 round(s) waiting with 1 of 1 runner slots in use")
    );
    assert_eq!(
        s.fields["slots"],
        json!([{ "pool": "proxmox", "in_use": 1, "budget": 1 }, { "pool": "ec2", "in_use": 0, "budget": 0 }])
    );
}

fn attempt(b: &Bench, hash: &str, n: u32, by: &str, at: u64) {
    b.seed(
        &format!("verdicts/lane/{hash}/attempt/{n}"),
        json!({ "class": "red", "step": "unit", "initiated_by": by, "at": at }),
    );
}

#[test]
fn a_planner_rerun_of_an_identity_still_red_is_a_repeat_and_judge_passes_are_their_own_term() {
    let b = Bench::new();
    lane(&b);
    b.seed(
        "verdicts/lane/h1",
        json!({ "class": "red", "step": "unit", "attempts": 3, "last_attempt": EPOCH_MS }),
    );
    attempt(&b, "h1", 1, "planner", EPOCH_MS - 30 * 60 * MIN);
    attempt(&b, "h1", 2, "judge", EPOCH_MS - 10 * MIN);
    attempt(&b, "h1", 3, "planner", EPOCH_MS - 5 * MIN);
    b.seed(
        "verdicts/lane/h2",
        json!({ "class": "green", "attempts": 2, "last_attempt": EPOCH_MS }),
    );
    attempt(&b, "h2", 1, "planner", EPOCH_MS - 26 * 60 * MIN);
    attempt(&b, "h2", 2, "planner", EPOCH_MS - 25 * 60 * MIN);
    b.seed("verdicts/lane/by-member/sp-a/a1", json!({ "hash": "h1" }));
    let s = built(&b);
    assert_eq!(
        level_of(&s, "repeat_identity_open"),
        1.0,
        "a repeat that went green is not open"
    );
    assert_eq!(
        s.fields["repeat_identity_passes_24h"],
        json!({ "value": 1.0, "window": "24h" })
    );
    assert_eq!(
        s.fields["judge_passes_24h"],
        json!({ "value": 1.0, "window": "24h" })
    );
    assert_eq!(s.repeat_identity_passes_24h, 1);
    assert_eq!(
        reason(&s, REPEAT_IDENTITY).as_deref(),
        Some("identity h1 run 2 times by the planner, still red")
    );
    assert_eq!(
        s.fields[REPEAT_IDENTITY_HOLDS],
        json!([{ "identity": "h1", "id": "sp-a", "tip": "a1" }]),
        "the open identity's proven member at its tip, for hold-member to name"
    );
}

#[test]
fn an_attempt_or_summary_that_does_not_decode_makes_the_section_unreadable() {
    for (key, data) in [
        ("verdicts/lane/h1", json!("garbled")),
        ("verdicts/lane/h1/attempt/one", json!({})),
        ("verdicts/lane/h1/attempt/1", json!({ "class": "red" })),
        ("verdicts/lane/by-member/sp-a/a1", json!({ "at": 1 })),
    ] {
        let b = Bench::new();
        lane(&b);
        b.seed(key, data);
        assert_eq!(
            build(&b.ports(), "lane").unwrap_err().word(),
            "unreadable",
            "{key}"
        );
    }
    let b = Bench::new();
    lane(&b);
    b.seed("verdicts/lane/h1/other/x", json!(1));
    assert!(
        build(&b.ports(), "lane").is_ok(),
        "a key the ledger does not keep is no attempt"
    );
}

#[test]
fn a_fault_on_the_lane_s_own_ref_and_image_blocks_it_and_every_fault_is_listed() {
    let b = Bench::new();
    lane(&b);
    b.seed(
        "faults/lane/other-base/sha256:img-0",
        json!({ "cause": { "cause": "base" }, "since": EPOCH_MS }),
    );
    let s = built(&b);
    assert_eq!(level_of(&s, "lanes_blocked"), 0.0);
    assert_eq!(
        s.fields["lane_faults"],
        json!([{ "key": "faults/lane/other-base/sha256:img-0", "cause": "base", "since": "2027-01-15T08:00:00Z" }])
    );
    b.seed(
        "faults/lane/base-0/sha256:img-0",
        json!({ "cause": { "cause": "image", "digest": "sha256:img-0" }, "since": EPOCH_MS }),
    );
    let blocked = built(&b);
    assert_eq!(level_of(&blocked, "lanes_blocked"), 1.0);
    let sec = section("lane", &blocked, None, EPOCH_MS);
    let kind = spira_desired_state::section::admit_publish(&sec).expect("admitted");
    let d = sec.distance_value(kind).unwrap().unwrap();
    let open = sec.distance.as_ref().unwrap().open;
    assert!(d > open, "a blocked lane with nothing else wrong opens its gap: {d} vs open {open}");
    b.seed("faults/lane/x/y", json!("garbled"));
    assert_eq!(build(&b.ports(), "lane").unwrap_err().word(), "unreadable");
}

#[test]
fn an_orphan_is_listed_with_its_re_drive_attempts() {
    let b = Bench::new();
    lane(&b);
    round(
        &b,
        "b1",
        "CiRunning",
        "base-0",
        json!({ "last_lease_token": { "main": 7, "sub": 0 } }),
    );
    b.seed(
        &orphans::record_key("lane", "b1"),
        json!({ "attempts": [{ "expired_token": { "main": 7, "sub": 0 }, "lease_token": { "main": 9, "sub": 0 },
                                "launch_result": "boom", "at_ms": EPOCH_MS, "next_attempt_at_ms": EPOCH_MS + MIN }] }),
    );
    let s = built(&b);
    assert_eq!(
        s.fields["orphans"],
        json!([{ "batch": "b1", "expired_token": "7.0", "redrive_attempts": 1, "failed_in_a_row": 1 }])
    );
    let b = Bench::new();
    lane(&b);
    round(
        &b,
        "b1",
        "CiRunning",
        "base-0",
        json!({ "last_lease_token": { "main": 7, "sub": 0 } }),
    );
    b.seed(&orphans::record_key("lane", "b1"), json!("garbled"));
    assert_eq!(build(&b.ports(), "lane").unwrap_err().word(), "unreadable");
}

#[test]
fn the_world_s_halt_and_a_scoped_stop_are_each_their_own_condition() {
    let b = Bench::new();
    lane(&b);
    assert_eq!(
        (reason(&built(&b), HALTED), reason(&built(&b), STOPPED)),
        (None, None)
    );
    b.stops.stop("lane", "ops", "drill", "manual");
    let s = built(&b);
    assert_eq!(
        reason(&s, STOPPED).as_deref(),
        Some("lane's rounds lane is stopped by ops: drill")
    );
    assert_eq!(reason(&s, HALTED), None);
    b.stops.halt();
    let s = built(&b);
    assert!(reason(&s, HALTED).unwrap().contains("world.halted"));
    assert_eq!(reason(&s, STOPPED), None);
    assert_eq!(raised(&s).into_iter().collect::<Vec<_>>(), [HALTED]);
}

#[test]
fn an_unreadable_store_is_a_refusal_not_an_empty_section() {
    let b = Bench::unserved();
    assert_eq!(build(&b.ports(), "lane").unwrap_err().word(), "unreadable");
    let r = publish(&b.ports(), "lane");
    assert_eq!(r.as_ref().unwrap_err().word(), "unreadable");
}

#[test]
fn a_section_is_written_on_change_and_otherwise_once_per_target_latency() {
    let b = Bench::new();
    stuck(&b);
    let p = b.ports();
    assert!(matches!(
        publish(&p, "lane").unwrap(),
        Publish::Written {
            green_unlanded_minutes: 10,
            ..
        }
    ));
    b.clock.advance(30_000);
    assert_eq!(publish(&p, "lane").unwrap(), Publish::Unchanged);
    b.clock.advance(TARGET_LATENCY_MS - 30_000);
    assert!(
        matches!(publish(&p, "lane").unwrap(), Publish::Written { .. }),
        "written again by target_latency"
    );
    assert_eq!(b.published().len(), 2);

    lease(&b, "round/lane/p1");
    b.clock.advance(1_000);
    assert!(
        matches!(publish(&p, "lane").unwrap(), Publish::Written { .. }),
        "a change is written at once"
    );
    let all = b.published();
    let cond = |s: &Value| s["status"]["conditions"][0].clone();
    assert_eq!(cond(&all[0])["status"], "True");
    assert_eq!(
        cond(&all[1])["lastTransitionTime"],
        cond(&all[0])["lastTransitionTime"],
        "a standing condition keeps its time"
    );
    assert_eq!(cond(&all[2])["status"], "False");
    assert_ne!(
        cond(&all[2])["lastTransitionTime"],
        cond(&all[0])["lastTransitionTime"]
    );
}

#[test]
fn a_spec_applied_by_compose_is_observed_by_the_next_publish() {
    let b = Bench::new();
    lane(&b);
    let p = b.ports();
    publish(&p, "lane").unwrap();
    let doc = json!({ "apiVersion": "spira/v1", "kind": NAME, "metadata": { "name": "lane" },
                      "spec": { "concurrency": { "value": 2, "statute": "law-x" } } });
    let (code, _) = b.call(&args(&[
        "section",
        "apply-spec",
        "--actor",
        "compose",
        "--json",
        &doc.to_string(),
    ]));
    assert_eq!(code, 0);
    assert!(matches!(
        publish(&p, "lane").unwrap(),
        Publish::Written { .. }
    ));
    assert_eq!(b.published()[1]["status"]["observedGeneration"], 1);
    let (_, rev) = published(&p, "lane").unwrap().unwrap();
    assert!(rev > 0);
}

/// Writes a competing copy of the section before forwarding each of its first `races` publishes.
struct Racing<'a> {
    b: &'a Bench,
    races: Mutex<usize>,
}

impl Lc for Racing<'_> {
    fn call(&self, a: &[String]) -> (i32, Value) {
        let publishing = a.get(1).map(String::as_str) == Some("publish-status");
        let mut races = crate::lock(&self.races);
        if publishing && *races > 0 {
            *races -= 1;
            let rev = published(&self.b.ports(), "lane")
                .unwrap()
                .map_or(0, |(_, r)| r);
            let mut racer = a.to_vec();
            let at = racer.iter().position(|x| x == "--expect-rev").unwrap() + 1;
            racer[at] = rev.to_string();
            assert_eq!(self.b.call(&racer).0, 0);
        }
        self.b.call(a)
    }
}

#[test]
fn a_write_another_moved_first_is_re_read_once_and_then_refused() {
    let b = Bench::new();
    lane(&b);
    let once = Racing {
        b: &b,
        races: Mutex::new(1),
    };
    let p = Ports {
        lc: &once,
        ..b.ports()
    };
    assert!(matches!(
        publish(&p, "lane").unwrap(),
        Publish::Written { .. }
    ));
    assert_eq!(
        b.published().len(),
        2,
        "the racer's write and the re-read one"
    );

    b.clock.advance(TARGET_LATENCY_MS);
    let always = Racing {
        b: &b,
        races: Mutex::new(2),
    };
    let p = Ports {
        lc: &always,
        ..b.ports()
    };
    let e = publish(&p, "lane").unwrap_err();
    assert!(e.to_string().contains("moved under two writes"), "{e}");
}

#[test]
fn a_malformed_status_is_refused_by_admission_and_the_refusal_is_logged_with_its_rule() {
    let b = Bench::new();
    stuck(&b);
    let p = b.ports();
    let mut s = section("lane", &built(&b), None, EPOCH_MS);
    s.status.fields.insert(
        "green_unlanded_minutes".into(),
        json!({ "value": 3.0, "window": "soon" }),
    );
    let r = write(&p, &s, 0).unwrap_err();
    let Unwritten::Refused(r) = r else {
        panic!("{r:?}")
    };
    assert_eq!(
        r,
        Refusal::Admission {
            key: "telemetry/sections/RoundController/lane".into(),
            field: "status.green_unlanded_minutes.window".into(),
            rule: "is neither level nor a duration".into(),
        }
    );
    report(&p, "lane", &Err(r.clone()));
    report(&p, "lane", &Err(r));
    let lines: Vec<String> = b.log.outcomes().iter().map(|o| o.line()).collect();
    assert_eq!(lines.len(), 1, "a standing refusal is told once: {lines:?}");
    assert!(
        lines[0].ends_with("declined admission-refused repo=lane batch=- key=telemetry/sections/RoundController/lane#status.green_unlanded_minutes.window rule=\"is neither level nor a duration\""),
        "{}",
        lines[0]
    );
    assert!(b.published().is_empty());
    assert_eq!(crate::lock(&b.lc_calls)[0].code, crate::ports::LC_REFUSED);
}

/// Answers each call with the next canned answer.
struct Canned(Mutex<Vec<(i32, Value)>>);

impl Lc for Canned {
    fn call(&self, _: &[String]) -> (i32, Value) {
        crate::lock(&self.0).remove(0)
    }
}

#[test]
fn a_door_that_cannot_answer_or_answers_nonsense_is_unreadable() {
    let b = Bench::new();
    lane(&b);
    let p = Ports {
        lc: &Unserved,
        ..b.ports()
    };
    assert!(published(&p, "lane")
        .unwrap_err()
        .to_string()
        .contains("no spira-lc door"));
    let canned = Canned(Mutex::new(vec![
        (0, json!({ "section": { "kind": 1 } })),
        (
            0,
            json!({ "section": { "apiVersion": "spira/v1", "kind": NAME, "metadata": { "name": "lane" } } }),
        ),
        (1, json!({ "not_found": "x" })),
        (2, json!({ "cannot_tell": "store down" })),
    ]));
    let p = Ports {
        lc: &canned,
        ..b.ports()
    };
    assert_eq!(published(&p, "lane").unwrap_err().word(), "unreadable");
    assert!(published(&p, "lane")
        .unwrap_err()
        .to_string()
        .contains("no resourceVersion"));
    assert_eq!(
        publish(&p, "lane"),
        Err(Refusal::unreadable(
            "telemetry/sections/RoundController/lane",
            json!({ "cannot_tell": "store down" })
        )),
        "nothing published yet, and the write cannot tell"
    );
}

#[test]
fn a_written_section_is_one_published_line_carrying_its_levels() {
    let b = Bench::new();
    stuck(&b);
    step(&b.ports(), "lane");
    let o = b.log.outcomes();
    assert_eq!(o.len(), 1);
    assert_eq!(o[0].kind, Kind::Published);
    assert!(o[0].line().ends_with("published section repo=lane batch=- green_unlanded_minutes=10 repeat_identity_passes_24h=0"), "{}", o[0].line());
}

#[test]
fn an_identity_whose_proven_member_is_held_at_its_tip_can_never_be_cut_again_and_is_not_open() {
    let b = Bench::new();
    lane(&b);
    for hash in ["h1", "h2"] {
        b.seed(
            &format!("verdicts/lane/{hash}"),
            json!({ "class": "red", "step": "unit", "attempts": 2, "last_attempt": EPOCH_MS }),
        );
        attempt(&b, hash, 1, "planner", EPOCH_MS - 20 * MIN);
        attempt(&b, hash, 2, "planner", EPOCH_MS - 10 * MIN);
    }
    b.seed("verdicts/lane/by-member/sp-a/a1", json!({ "hash": "h1" }));
    b.seed("verdicts/lane/by-member/sp-b/b1", json!({ "hash": "h2" }));
    let hold = |id: &str, tip: &str| {
        crate::knobs::hold_key(
            "lane",
            &crate::ports::Member {
                id: id.into(),
                tip: tip.into(),
            },
        )
    };
    b.seed(&hold("sp-a", "a1"), json!({ "lifted_by": "reconciler" }));
    b.seed(&hold("sp-b", "b0"), json!({ "lifted_by": "reconciler" }));
    let s = built(&b);
    assert_eq!(
        level_of(&s, "repeat_identity_open"),
        1.0,
        "h1's member is held at the tip it was proven at; h2's hold is at another tip"
    );
    assert_eq!(
        reason(&s, REPEAT_IDENTITY).as_deref(),
        Some("identity h2 run 2 times by the planner, still red")
    );
    assert_eq!(
        s.fields[REPEAT_IDENTITY_HOLDS],
        json!([{ "identity": "h2", "id": "sp-b", "tip": "b1" }]),
        "a held identity offers no member to hold"
    );
}
