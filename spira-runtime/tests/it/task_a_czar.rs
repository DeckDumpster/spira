//! Acceptance T1–T6 for `czar`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/czar.md`, Acceptance) on `TaskRig`. Red until the port
//! lands: `decide` and `advance` are `unimplemented!`, and the row waits in `tasks::a::PENDING`.
//! T6 runs two virtual days (the contract asks seven).

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: czar,
    registration: Czar::new(policy(Stage::Shadow)).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: 30_000,
    test_ids: [ #[test_id(t53b4ab)] #[test_id(tiq40zc)] #[test_id(tbu57i3)] #[test_id(tjtsx3i)] #[test_id(tnrwnsk)] ],
}

const NOW: u64 = 1_793_000_000_000;

/// Test thresholds, none of them a shipped value; every class at `stage`.
fn policy(stage: Stage) -> Policy {
    Policy {
        stages: CLASSES
            .iter()
            .chain(&["starved"])
            .map(|c| (c.to_string(), stage))
            .collect(),
        loop_stall_secs: 900,
        ci_queued_max_secs: 1_800,
        ci_red_max_secs: 3_600,
        base_ci_unreadable_grace_secs: 600,
        reconciler_grace_secs: 600,
        queue_round_min_n: 2,
        queue_round_stall_secs: 1_800,
    }
}

/// The contract's idle fixture: every key satisfied, no open batch, a fresh landing pass.
fn idle() -> Snapshot {
    Snapshot {
        landing_events: Read::Observed(vec![]),
        last_landing_pass_at_ms: Read::Observed(Some(NOW - 60_000)),
        open_batch: [("spira".to_string(), Read::Observed(None))].into(),
        certified: [("spira".to_string(), Read::Observed(vec![]))].into(),
        open_rounds: [("spira".to_string(), Read::Observed(0))].into(),
        batcher_held: Default::default(),
        ci: [("spira".to_string(), Read::Observed(Ci::default()))].into(),
        base_ci: [(
            "spira".to_string(),
            Read::Observed(BaseCi {
                conclusion: Some("success".into()),
                head_sha: "b1".into(),
                ..BaseCi::default()
            }),
        )]
        .into(),
        reconciler_last_pass_ms: Read::Observed(Some(NOW - 30_000)),
        now_ms: NOW,
        gh_budget: None,
        mark: Mark::default(),
    }
}

/// `no suites identified; leaving batch open` with the batch still open: a deadlock.
fn busy() -> Snapshot {
    Snapshot {
        landing_events: Read::Observed(vec![LandingEvent {
            kind: "verdict.deadlock".into(),
            repo: "spira".into(),
            at_ms: NOW - 10_000,
        }]),
        open_batch: [(
            "spira".to_string(),
            Read::Observed(Some(Batch {
                pr: Some(41),
                branch: "spira/batch-1".into(),
                members: vec!["sp-a".into(), "sp-b".into()],
            })),
        )]
        .into(),
        ..idle()
    }
}

#[test_id(to9ay33)]
#[test]
fn t3_every_rung_of_the_ladder_is_taken() {
    for (from, to) in LADDER {
        assert_eq!(
            advance("deadlock", *from, *to),
            Ok(*to),
            "{from:?} → {to:?}"
        );
    }
}

#[test_id(tx13oy0)]
#[test]
fn t3_inferred_without_a_gap_is_refused_naming_the_key() {
    let refused = advance("ci-red:spira", Phase::Satisfied, Phase::Inferred)
        .expect_err("inferred needs a gap first");
    assert!(refused.contains("ci-red:spira"), "{refused}");
}

#[test_id(tmb16zx)]
#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(30_000).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `cp:312-320`: in shadow a deadlock is a `CZAR-WOULD` line, never an incident.
#[test_id(t39hxdm)]
#[test]
fn t5_a_shadow_deadlock_is_a_would_line_and_files_nothing() {
    let (carry, acts) = decide(&policy(Stage::Shadow), &Carry::default(), &busy());
    assert_eq!(
        carry.keys.get("deadlock").map(|k| k.phase),
        Some(Phase::Gap)
    );
    assert!(
        acts.iter()
            .any(|a| a.effect == INBOX_APPEND && a.input.contains("CZAR-WOULD")),
        "{acts:?}"
    );
    assert!(acts.iter().all(|a| a.effect != FILE_INCIDENT), "{acts:?}");
}

/// `cp:332-343`: in act a deadlock files one incident keyed `incident:queue-<ref>`.
#[test_id(txbknif)]
#[test]
fn t5_an_act_deadlock_files_one_incident_by_its_ref() {
    let (_, acts) = decide(&policy(Stage::Act), &Carry::default(), &busy());
    let filed: Vec<&Act> = acts.iter().filter(|a| a.effect == FILE_INCIDENT).collect();
    assert_eq!(filed.len(), 1, "{acts:?}");
    assert!(
        filed[0].key.starts_with("incident:queue-"),
        "{:?}",
        filed[0]
    );
}

/// `cp:369-395`: a red base files P0 against the red repo, keyed `incident:base-red:<repo>:<suites>`.
#[test_id(tbge2hl)]
#[test]
fn t5_a_red_base_files_base_red_against_the_repo() {
    let red = Snapshot {
        base_ci: [(
            "spira".to_string(),
            Read::Observed(BaseCi {
                conclusion: Some("failure".into()),
                head_sha: "b2".into(),
                run_url: Some("u".into()),
                red_suites: vec!["lint".into()],
                ..BaseCi::default()
            }),
        )]
        .into(),
        ..idle()
    };
    let (_, acts) = decide(&policy(Stage::Act), &Carry::default(), &red);
    assert!(
        acts.iter()
            .any(|a| a.effect == FILE_BASE_RED && a.key == "incident:base-red:spira:lint"),
        "{acts:?}"
    );
}

/// sp-pu7v6: an unreadable field is never satisfied.
#[test_id(t8iccjd)]
#[test]
fn t5_an_unobservable_ci_is_not_satisfied() {
    let blind = Snapshot {
        ci: [(
            "spira".to_string(),
            Read::Unobservable("forge timed out".into()),
        )]
        .into(),
        ..idle()
    };
    let (carry, _) = decide(&policy(Stage::Shadow), &Carry::default(), &blind);
    assert_ne!(
        carry.keys.get("ci-stalled:spira").map(|k| k.phase),
        Some(Phase::Satisfied)
    );
}

/// law-lifecycle-state-is-not-text (sp-mijstw): the certified pool counts only unheld rows, and a
/// hold written as the JSON-encoded string spira-lc may emit is still a hold.
#[test_id(t27h598)]
#[test]
fn t7_a_hold_in_string_form_keeps_its_row_out_of_the_certified_pool() {
    let list = r#"[{"bead_id":"sp-a","state":"CERTIFIED","holds":[]},{"bead_id":"sp-b","state":"CERTIFIED","holds":"[\"poison\"]"},{"bead_id":"sp-c","state":"CERTIFIED","holds":["ask"]},{"bead_id":"sp-d","state":"CERTIFIED","holds":"[]"}]"#;
    assert_eq!(
        unheld_ids(list),
        Ok(vec!["sp-a".to_string(), "sp-d".to_string()])
    );
    assert!(unheld_ids("cannot tell: deadline").is_err());
}

fn at_work() -> TaskRig {
    let mut r = rig(1, busy());
    r.advance(30_000).unwrap();
    r
}

#[test_id(tapo3mb)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&at_work(), NAME);
    assert!(
        rows.iter().any(|k| k.starts_with("keys/deadlock")),
        "{rows:?}"
    );
    assert!(rows.iter().any(|k| k == "cursor_ms"), "{rows:?}");
}

#[test_id(tgxe5sh)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(
        at_work,
        registration,
        STREAM,
        EventKind::new(TICK, ""),
    );
}

/// A spent GitHub budget is one page with its reset and the top spender, not one per repository.
#[test_id(thh57vh)]
#[test]
fn a_spent_budget_files_one_page_naming_the_reset_and_the_top_spender() {
    let blind = |repo: &str| {
        (
            repo.to_string(),
            Read::Unobservable("forge exited 1".into()),
        )
    };
    let spent = Snapshot {
        base_ci: [blind("spira"), blind("other")].into(),
        gh_budget: Some(SpentBudget {
            reset: NOW / 1000 + 600,
            top_spender: Some(("forge:pr-state".into(), 900)),
        }),
        ..idle()
    };
    let p = Policy {
        base_ci_unreadable_grace_secs: 0,
        ..policy(Stage::Act)
    };
    let (_, acts) = decide(&p, &Carry::default(), &spent);
    let filed: Vec<&Act> = acts.iter().filter(|a| a.effect == FILE_BASE_RED).collect();
    assert_eq!(filed.len(), 1, "{acts:?}");
    let text = format!("{:?}", filed[0]);
    assert!(
        text.contains("forge:pr-state")
            && text.contains(&spira_runtime::api_budget::clock_of(NOW / 1000 + 600)),
        "{text}"
    );
}

const LOG_PASS: &str = "2026-11-20T10:00:00Z landing: pass complete";
const LOG_SORT: &str = "queue_sort_rows: ranking failed";

fn stamped(secs_into_day: u32, text: &str) -> String {
    format!(
        "2026-11-21T{:02}:{:02}:{:02}Z {text}",
        secs_into_day / 3600,
        secs_into_day / 60 % 60,
        secs_into_day % 60
    )
}

/// One observe over `run`, fed through `decide` as the scheduler does; returns the next carry and what it saw.
fn pass(run: &std::path::Path, carry: &Carry) -> (Carry, Snapshot) {
    let e = spira_runtime::Effects {
        world: spira_runtime::world::World {
            run_dir: run.to_path_buf(),
            ..Default::default()
        },
        ..Default::default()
    };
    let input = serde_json::to_string(&ObserveIn {
        carry: carry.clone(),
        pinned: Default::default(),
    })
    .unwrap();
    let snap: Snapshot = serde_json::from_str(&observe(&e, &input).unwrap()).unwrap();
    let snap = Snapshot {
        open_batch: idle().open_batch,
        certified: idle().certified,
        ..snap
    };
    (decide(&policy(Stage::Shadow), carry, &snap).0, snap)
}

fn events(s: &Snapshot) -> Vec<LandingEvent> {
    match &s.landing_events {
        Read::Observed(e) => e.clone(),
        other => panic!("{other:?}"),
    }
}

#[test_id(tvmfuis)]
#[test]
fn an_unchanged_log_is_not_read_again_even_when_a_line_changed_in_place() {
    let dir = testkit::TempDir::new("czar-log-unchanged");
    let log = dir.path().join("landing.log");
    let old = stamped(100, LOG_SORT);
    std::fs::write(&log, format!("{old}\n{LOG_PASS}\n")).unwrap();
    let (carry, first) = pass(dir.path(), &Carry::default());
    assert_eq!(events(&first).len(), 1);
    let newer = stamped(200, LOG_SORT);
    assert_eq!(newer.len(), old.len());
    let f = std::fs::OpenOptions::new().write(true).open(&log).unwrap();
    std::os::unix::fs::FileExt::write_all_at(&f, newer.as_bytes(), 0).unwrap();
    let (_, second) = pass(dir.path(), &carry);
    assert!(events(&second).is_empty(), "{second:?}");
    assert_eq!(
        second.last_landing_pass_at_ms,
        first.last_landing_pass_at_ms
    );
}

#[test_id(t261hfa)]
#[test]
fn an_appended_event_is_reported_once_and_an_unterminated_one_only_when_completed() {
    use std::io::Write;
    let dir = testkit::TempDir::new("czar-log-append");
    let log = dir.path().join("landing.log");
    std::fs::write(&log, format!("{LOG_PASS}\n")).unwrap();
    let (carry, _) = pass(dir.path(), &Carry::default());
    let line = stamped(300, LOG_SORT);
    let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    f.write_all(line.as_bytes()).unwrap();
    let (carry, partial) = pass(dir.path(), &carry);
    assert!(events(&partial).is_empty(), "{partial:?}");
    f.write_all(b"\n").unwrap();
    let (carry, done) = pass(dir.path(), &carry);
    assert_eq!(events(&done).len(), 1, "{done:?}");
    let (_, again) = pass(dir.path(), &carry);
    assert!(events(&again).is_empty(), "{again:?}");
}

#[test_id(t8p276x)]
#[test]
fn a_truncated_or_replaced_log_is_read_again_from_the_start() {
    let dir = testkit::TempDir::new("czar-log-rotate");
    let log = dir.path().join("landing.log");
    let filler = "x".repeat(200);
    std::fs::write(
        &log,
        format!("{}\n{filler}\n{filler}\n", stamped(100, LOG_SORT)),
    )
    .unwrap();
    let (carry, _) = pass(dir.path(), &Carry::default());
    std::fs::write(&log, format!("{}\n", stamped(200, LOG_SORT))).unwrap();
    let (carry, truncated) = pass(dir.path(), &carry);
    assert_eq!(events(&truncated).len(), 1, "{truncated:?}");
    let next = dir.path().join("landing.log.new");
    std::fs::write(&next, format!("{}\n", stamped(300, LOG_SORT))).unwrap();
    assert!(
        std::fs::metadata(&next).unwrap().len() <= std::fs::metadata(&log).unwrap().len(),
        "only the inode may tell"
    );
    std::fs::rename(&next, &log).unwrap();
    let (_, replaced) = pass(dir.path(), &carry);
    let seen = events(&replaced);
    assert_eq!(seen.len(), 1, "{replaced:?}");
    assert!(seen[0].at_ms > events(&truncated)[0].at_ms);
}

#[test_id(tkvrt22)]
#[test]
fn with_no_mark_the_first_pass_finds_the_last_pass_anywhere_in_the_file() {
    let dir = testkit::TempDir::new("czar-log-first");
    let mut text = format!("{LOG_PASS}\n");
    for _ in 0..50 {
        text.push_str("noise\n");
    }
    std::fs::write(dir.path().join("landing.log"), text).unwrap();
    let (carry, first) = pass(dir.path(), &Carry::default());
    assert!(
        matches!(first.last_landing_pass_at_ms, Read::Observed(Some(_))),
        "{first:?}"
    );
    let (_, second) = pass(dir.path(), &carry);
    assert_eq!(
        second.last_landing_pass_at_ms, first.last_landing_pass_at_ms,
        "a pass with no new bytes keeps the carried reading"
    );
}

#[test_id(t5d9sle)]
#[test]
fn a_carried_last_pass_that_ages_past_the_stall_still_pages() {
    let dir = testkit::TempDir::new("czar-log-stall");
    std::fs::write(dir.path().join("landing.log"), format!("{LOG_PASS}\n")).unwrap();
    let (carry, first) = pass(dir.path(), &Carry::default());
    let (_, second) = pass(dir.path(), &carry);
    assert_eq!(
        second.last_landing_pass_at_ms,
        first.last_landing_pass_at_ms
    );
    assert!(matches!(
        second.last_landing_pass_at_ms,
        Read::Observed(Some(_))
    ));
    let late = Snapshot {
        now_ms: NOW + 100_000_000_000,
        ..second
    };
    let (_, acts) = decide(&policy(Stage::Act), &Carry::default(), &late);
    assert!(
        acts.iter()
            .any(|a| a.effect == FILE_INCIDENT && a.key.contains("loop-stalled")),
        "{acts:?}"
    );
}

#[test_id(t9gxaud)]
#[test]
fn a_missing_log_is_no_events_no_last_pass_and_an_empty_mark() {
    let dir = testkit::TempDir::new("czar-log-missing");
    let had = Carry {
        mark: Mark {
            dev: 1,
            ino: 2,
            offset: 9,
            last_pass: Read::Observed(Some(5)),
        },
        ..Carry::default()
    };
    let (carry, snap) = pass(dir.path(), &had);
    assert!(events(&snap).is_empty());
    assert_eq!(snap.last_landing_pass_at_ms, Read::Observed(None));
    assert_eq!(carry.mark, Mark::default());
}

#[test_id(t8rqfa2)]
#[test]
fn an_unreadable_log_leaves_the_mark_and_reads_both_unobservable() {
    let dir = testkit::TempDir::new("czar-log-unreadable");
    std::fs::create_dir(dir.path().join("landing.log")).unwrap();
    let had = Carry {
        mark: Mark {
            dev: 1,
            ino: 2,
            offset: 9,
            last_pass: Read::Observed(Some(5)),
        },
        ..Carry::default()
    };
    let (carry, snap) = pass(dir.path(), &had);
    assert!(
        matches!(snap.landing_events, Read::Unobservable(_)),
        "{snap:?}"
    );
    assert!(
        matches!(snap.last_landing_pass_at_ms, Read::Unobservable(_)),
        "{snap:?}"
    );
    assert_eq!(carry.mark, had.mark);
}

#[test_id(txxqtbs)]
#[test]
fn a_carried_unobservable_last_pass_survives_new_bytes_with_no_pass_line() {
    use std::io::Write;
    let dir = testkit::TempDir::new("czar-log-carried-unobservable");
    let log = dir.path().join("landing.log");
    std::fs::write(&log, "short landing: pass complete\n").unwrap();
    let (carry, first) = pass(dir.path(), &Carry::default());
    assert!(
        matches!(first.last_landing_pass_at_ms, Read::Unobservable(_)),
        "{first:?}"
    );
    std::fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(b"noise\n")
        .unwrap();
    let (_, second) = pass(dir.path(), &carry);
    assert_eq!(
        second.last_landing_pass_at_ms,
        first.last_landing_pass_at_ms
    );
}

#[test_id(tizquhw)]
#[test]
fn the_mark_is_kept_as_a_row_and_joins_back() {
    let mark = Mark {
        dev: 3,
        ino: 4,
        offset: 5,
        last_pass: Read::Observed(Some(6)),
    };
    let body = serde_json::to_string(&Carry {
        mark: mark.clone(),
        ..Carry::default()
    })
    .unwrap();
    let rows = runtime::rows::split(&body, ROW_PARTS);
    assert!(rows.contains_key("mark"), "{rows:?}");
    let back: Carry = serde_json::from_str(&runtime::rows::join(&rows, ROW_PARTS)).unwrap();
    assert_eq!(back.mark, mark);
}
