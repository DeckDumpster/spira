//! Acceptance T1–T6 for `landing`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/landing.md`, Acceptance) on `TaskRig`. Red until the port
//! lands: `decide`, `step` and `exit_code` are `unimplemented!`, and the row waits in
//! `tasks::a::PENDING`. T6 runs two virtual days (the contract asks three, with scripted PR states).

use std::collections::BTreeMap;

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: landing,
    registration: Landing::new(BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: CADENCE_MS,
    test_ids: [ #[test_id(tmwgzzj)] #[test_id(tw01ecd)] #[test_id(tk9b3tu)] #[test_id(tvd18zt)] #[test_id(tumnbn7)] ],
}

fn bead(id: &str, handed_on: bool) -> BeadRow {
    BeadRow { id: id.into(), repo: "pokedumpster".into(), handed_on, priority: 2, version: 4, ..BeadRow::default() }
}

fn branch(id: &str, handed_on: bool) -> Branch {
    Branch { br: format!("spira/{id}"), tip: format!("{id}-tip"), bead: Some(bead(id, handed_on)), lc_state: Some("PR_OPEN".into()), pr_state: PrState::None, ..Branch::default() }
}

fn with(branches: Vec<Branch>) -> Snapshot {
    Snapshot { forge_enabled: true, at: 1_000, repos: vec![Repo { name: "pokedumpster".into(), path: "r/poke".into(), landref: "main".into(), base_fq: "origin/main".into(), base_sha: "b1".into(), branches }] }
}

/// The contract's idle fixture: forge enabled, one pr repo, two branches, neither handed on.
fn idle() -> Snapshot {
    with(vec![branch("pd-a", false), branch("pd-b", false)])
}

/// One branch handed on with no PR yet: the branch machine runs.
fn busy() -> Snapshot {
    with(vec![branch("pd-a", true), branch("pd-b", false)])
}

fn effects(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

#[test_id(tka9jbe)]
#[test]
fn t3_every_row_of_the_branch_machine_is_taken_and_keeps_its_exit_code() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
    for (from, on, code) in EXIT {
        assert_eq!(exit_code(*from, *on), Some(*code), "{from:?} × {on:?}");
    }
}

#[test_id(tf35zpx)]
#[test]
fn t3_pushing_x_pr_merged_is_refused_naming_pushing() {
    let refused = step(State::Pushing, Trigger::PrMerged).expect_err("pushing × pr_state=merged is illegal");
    assert!(refused.to_lowercase().contains("pushing"), "{refused}");
}

#[test_id(tw0zqt1)]
#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `pr.rs:111-170`: a branch handed on with no PR is rebased; one not handed on is skipped.
#[test_id(tg373ml)]
#[test]
fn t5_a_handed_on_branch_is_rebased_and_the_rest_skipped() {
    let (carry, acts) = decide(&Carry::default(), &busy());
    assert_eq!(acts.iter().filter(|a| a.effect == REBASE).map(|a| a.key.as_str()).collect::<Vec<_>>(), ["spira/pd-a/b1"]);
    assert_eq!(carry.branches.get("spira/pd-a").map(|b| b.state), Some(State::Rebasing));
    assert!(!carry.branches.contains_key("spira/pd-b"));
}

/// `pr_branch.rs:38-56`: a PR merged while not handed on is delivered.
#[test_id(tol2f0g)]
#[test]
fn t5_a_pr_merged_while_not_handed_on_is_delivered() {
    let merged = Branch { pr_state: PrState::Merged, merge_sha: Some("m1".into()), ..branch("pd-a", false) };
    let (_, acts) = decide(&Carry::default(), &with(vec![merged]));
    assert!(acts.iter().any(|a| a.effect == DELIVER_PR_MERGED && a.key == "pd-a/m1"), "{acts:?}");
}

/// `pr.rs:181-213`: content already on base is delivered by content, not pushed.
#[test_id(t2btbfe)]
#[test]
fn t5_content_already_on_base_is_delivered_by_content() {
    let landed = Branch { content_on_base: true, ..branch("pd-a", true) };
    let acts = decide(&Carry::default(), &with(vec![landed])).1;
    assert!(effects(&acts).contains(&DELIVER_BY_CONTENT) && !effects(&acts).contains(&REBASE), "{acts:?}");
}

/// A live holder defers the branch: nothing is done to it.
#[test_id(t7gkgcw)]
#[test]
fn t5_a_live_holder_defers() {
    let held = Branch { holder_alive: true, ..branch("pd-a", true) };
    assert_eq!(decide(&Carry::default(), &with(vec![held])).1, Vec::<Act>::new());
}

/// `pr.rs:56-59`: forge disabled is idle.
#[test_id(tia17zt)]
#[test]
fn t5_forge_disabled_does_nothing() {
    assert_eq!(decide(&Carry::default(), &Snapshot { forge_enabled: false, ..busy() }).1, Vec::<Act>::new());
}

/// law-lifecycle-state-is-not-text (sp-mijstw): a rebase step's outcome is a typed body, never
/// the first word of text. A rebase whose effect failed (a timeout, `rebase-stale` usage/config
/// exit 2, a tool that never ran) is no verdict on the branch: it ends the pass without sending
/// the bead back to rework. Only a reported conflict reopens it.
fn rebasing(id: &str) -> (Carry, String) {
    let leg = serde_json::json!({"repo": "pokedumpster", "path": "r/poke", "landref": "main", "base_fq": "origin/main", "base_sha": "b1", "br": format!("spira/{id}"), "id": id, "tip": format!("{id}-tip"), "pr_state": "none", "version": 4});
    let mut carry = Carry::default();
    carry.branches.insert(format!("spira/{id}"), BranchCarry { state: State::Rebasing, ..BranchCarry::default() });
    (carry, leg.to_string())
}

#[test_id(ta4ywgb)]
#[test]
fn t7_a_rebase_that_could_not_run_is_not_a_conflict() {
    let (carry, leg) = rebasing("pd-a");
    let (next, acts) = on_outcome(&carry, REBASE, &leg, false, "landing.rebase: rebase-stale timed out");
    assert!(!effects(&acts).contains(&REOPEN), "an infra failure sent the bead to rework: {acts:?}");
    assert_eq!(next.branches["spira/pd-a"].state, State::Done);
}

#[test_id(tg4ymac)]
#[test]
fn t7_rebase_outcomes_are_read_from_the_typed_body() {
    let (carry, leg) = rebasing("pd-a");
    let (next, acts) = on_outcome(&carry, REBASE, &leg, true, r#"{"outcome":"ok"}"#);
    assert_eq!(effects(&acts), vec![CONFINE], "ok rebases go on to confine");
    assert_eq!(next.branches["spira/pd-a"].state, State::Confining);
    let (_, acts) = on_outcome(&carry, REBASE, &leg, true, r#"{"outcome":"conflict","detail":"src/a.rs"}"#);
    assert_eq!(effects(&acts), vec![REOPEN], "a reported conflict reopens");
    let (_, acts) = on_outcome(&carry, REBASE, &leg, true, "ok but not the body");
    assert!(!effects(&acts).contains(&REOPEN), "an unreadable body is no verdict: {acts:?}");
}

#[test_id(ta23g9q)]
#[test]
fn t7_confine_outcomes_are_read_from_the_typed_body() {
    let (mut carry, leg) = rebasing("pd-a");
    carry.branches.get_mut("spira/pd-a").unwrap().state = State::Confining;
    let (next, acts) = on_outcome(&carry, CONFINE, &leg, true, r#"{"status":0,"detail":""}"#);
    assert_eq!(effects(&acts), vec![FORCE_PUSH]);
    assert_eq!(next.branches["spira/pd-a"].state, State::Pushing);
    let (_, acts) = on_outcome(&carry, CONFINE, &leg, true, r#"{"status":2,"detail":"touches gate.steps"}"#);
    assert_eq!(effects(&acts), vec![REOPEN]);
    let (next, acts) = on_outcome(&carry, CONFINE, &leg, true, "0 clean");
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(next.branches["spira/pd-a"].state, State::Done, "an unreadable confine body is inconclusive");
}

fn at_work() -> TaskRig {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    r
}

#[test_id(tlqi35j)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&at_work(), NAME);
    assert!(rows.iter().any(|k| k.starts_with("branches/")), "{rows:?}");
}

#[test_id(ttgmu4c)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(at_work, registration, STREAM, EventKind::new(TICK, ""));
}

mod deliveries {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use spira_runtime::tasks::a::landing::{deliver_by_content_on, deliver_pr_closed_on, deliver_pr_merged_on};
    use spira_store::lc_call::{LcClient, Reply, Transport};

    /// The store as the real socket answers it: `deliver` is refused with usage, `show` and `event` are served.
    struct Socket {
        row: String,
        seen: Mutex<Vec<String>>,
    }

    impl Transport for Socket {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
            self.seen.lock().unwrap().push(argv.join(" "));
            let (code, out) = match argv[0].as_str() {
                "show" => (0, self.row.clone()),
                "event" => (0, String::new()),
                _ => (2, "usage: spira-lc show <bead-id> | …".into()),
            };
            Ok(Reply { code, out, err: String::new(), read_ended: false })
        }
    }

    fn row(delivery: &str) -> String {
        format!(r#"{{"bead":{{"state":"SUBMITTED","version":12}},"delivery":{delivery}}}"#)
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `main` with a base commit; `spira/sp-a` adds `f`, and main takes it as one squash commit.
    fn squash_merged(d: &Path) -> String {
        git(d, &["init", "-q", "-b", "main"]);
        std::fs::write(d.join("base"), "base\n").unwrap();
        git(d, &["add", "."]);
        git(d, &["commit", "-qm", "base"]);
        git(d, &["checkout", "-qb", "spira/sp-a"]);
        std::fs::write(d.join("f"), "change\n").unwrap();
        git(d, &["add", "."]);
        git(d, &["commit", "-qm", "sp-a: the change"]);
        git(d, &["checkout", "-q", "main"]);
        git(d, &["merge", "--squash", "spira/sp-a"]);
        git(d, &["commit", "-qm", "sp-a: the change (#1)"]);
        git(d, &["rev-parse", "HEAD"])
    }

    fn leg(d: &Path, tip: &str) -> String {
        serde_json::json!({"repo": "r", "path": d.display().to_string(), "landref": "main", "base_fq": "main", "base_sha": tip, "br": "spira/sp-a", "id": "sp-a", "tip": tip, "pr_state": "merged", "version": 4}).to_string()
    }

    fn client(row: String) -> (Arc<Socket>, LcClient) {
        let s = Arc::new(Socket { row, seen: Mutex::new(Vec::new()) });
        (s.clone(), LcClient::over(s, true))
    }

    
    #[test_id(tbbomd8)]
    #[test]
    fn deliver_by_content_lands_a_squash_merged_branch_with_no_delivery_row_by_an_event() {
        let d = testkit::TempDir::new("landing-deliver");
        let sha = squash_merged(d.path());
        let (s, c) = client(row("null"));
        deliver_by_content_on(&c, &leg(d.path(), &sha)).unwrap();
        let want = format!(r#"event bead sp-a --expect SUBMITTED --version 12 --actor pr-pass-branch --kind {{"ContentOnBase":{{"proof":"merge-tree:{sha}"}}}}"#);
        assert_eq!(*s.seen.lock().unwrap(), vec!["show sp-a".to_string(), want]);
    }

    
    #[test_id(tri0n9e)]
    #[test]
    fn deliver_pr_merged_records_a_merged_branch_and_a_pr_open_row_gets_the_delivery_event() {
        let d = testkit::TempDir::new("landing-deliver");
        let sha = squash_merged(d.path());
        let (s, c) = client(row("null"));
        deliver_pr_merged_on(&c, &leg(d.path(), &sha)).unwrap();
        assert!(s.seen.lock().unwrap()[1].contains(&format!("merge-tree:{sha}")));
        let (s, c) = client(row(r#"{"state":"PR_OPEN","version":9}"#));
        deliver_pr_merged_on(&c, &leg(d.path(), &sha)).unwrap();
        let seen = s.seen.lock().unwrap().clone();
        assert!(seen[1].starts_with("event delivery sp-a --expect PR_OPEN --version 9 --actor pr-pass-branch"), "{seen:?}");
        assert!(seen[1].contains(&format!(r#""merge_sha":"{sha}""#)) && seen[1].contains(r#""proof":"merge-tree""#), "{seen:?}");
    }

    
    #[test_id(tnqgrs8)]
    #[test]
    fn deliver_pr_merged_sends_no_event_for_a_change_reverted_on_the_base() {
        let d = testkit::TempDir::new("landing-deliver");
        squash_merged(d.path());
        git(d.path(), &["revert", "--no-edit", "HEAD"]);
        let reverted = git(d.path(), &["rev-parse", "HEAD"]);
        let (s, c) = client(row("null"));
        let e = deliver_pr_merged_on(&c, &leg(d.path(), &reverted)).unwrap_err();
        assert!(e.contains("sp-a") && e.contains(&reverted) && e.contains("its change is not in force on main"), "{e}");
        assert_eq!(*s.seen.lock().unwrap(), vec!["show sp-a"]);
    }

    
    #[test_id(tkith7w)]
    #[test]
    fn deliver_pr_merged_records_a_deleted_branch_whose_merge_commit_is_on_head_by_ancestry() {
        let d = testkit::TempDir::new("landing-deliver");
        let sha = squash_merged(d.path());
        git(d.path(), &["branch", "-D", "spira/sp-a"]);
        let (s, c) = client(row("null"));
        deliver_pr_merged_on(&c, &leg(d.path(), &sha)).unwrap();
        assert!(s.seen.lock().unwrap()[1].contains(&format!(r#""proof":"ancestry:{sha}""#)), "{:?}", s.seen.lock().unwrap());
    }

    #[test_id(t22z3fg)]
    #[test]
    fn deliver_pr_closed_returns_a_pr_open_row_and_deliver_by_content_leaves_another_delivery_state_alone() {
        let d = testkit::TempDir::new("landing-deliver");
        let sha = squash_merged(d.path());
        let (s, c) = client(row(r#"{"state":"PR_OPEN","version":9}"#));
        deliver_pr_closed_on(&c, &leg(d.path(), &sha)).unwrap();
        let seen = s.seen.lock().unwrap().clone();
        assert!(seen[1].starts_with("event delivery sp-a --expect PR_OPEN --version 9 --actor pr-pass-branch") && seen[1].contains("pr-closed-unmerged"), "{seen:?}");
        let (s, c) = client(row(r#"{"state":"BATCHED","version":9}"#));
        deliver_by_content_on(&c, &leg(d.path(), &sha)).unwrap();
        assert_eq!(*s.seen.lock().unwrap(), vec!["show sp-a"]);
    }
}

fn with_pr(id: &str, number: u64, status: PrStatus) -> Branch {
    Branch { pr_state: PrState::Open, pr: Some(PrRead { number, status, since: 1_000 }), ..branch(id, true) }
}

#[test_id(t2dhvq9)]
#[test]
fn check_lines_classify_running_green_red_and_unread() {
    assert_eq!(checks_status(""), PrStatus::Unread);
    assert_eq!(checks_status("COMPLETED SUCCESS a\nCOMPLETED NEUTRAL b\nCOMPLETED SKIPPED c"), PrStatus::Green);
    assert_eq!(checks_status("COMPLETED SUCCESS a\nIN_PROGRESS - b"), PrStatus::Running);
    assert_eq!(checks_status("QUEUED - a"), PrStatus::Running);
    assert_eq!(checks_status("COMPLETED FAILURE a\nCOMPLETED SUCCESS b"), PrStatus::Red);
    assert_eq!(checks_status("COMPLETED CANCELLED a"), PrStatus::Red);
    assert_eq!(checks_status("COMPLETED FAILURE a\nIN_PROGRESS - b"), PrStatus::Running, "an unfinished check keeps the pull request running");
}

#[test_id(to23f7j)]
#[test]
fn since_is_kept_while_number_and_status_hold_and_reset_when_either_changes() {
    let first = Snapshot { at: 1_000, ..with(vec![with_pr("pd-a", 5, PrStatus::Running)]) };
    let (carry, _) = decide(&Carry::default(), &first);
    assert_eq!(carry.prs["pd-a"], PrRead { number: 5, status: PrStatus::Running, since: 1_000 });
    let later = Snapshot { at: 2_000, ..with(vec![with_pr("pd-a", 5, PrStatus::Running)]) };
    let (kept, _) = decide(&carry, &later);
    assert_eq!(kept.prs["pd-a"].since, 1_000);
    let (red, _) = decide(&kept, &Snapshot { at: 3_000, ..with(vec![with_pr("pd-a", 5, PrStatus::Red)]) });
    assert_eq!(red.prs["pd-a"], PrRead { number: 5, status: PrStatus::Red, since: 3_000 });
    let (renumbered, _) = decide(&red, &Snapshot { at: 4_000, ..with(vec![with_pr("pd-a", 6, PrStatus::Red)]) });
    assert_eq!(renumbered.prs["pd-a"].since, 4_000);
    let (gone, _) = decide(&renumbered, &Snapshot { at: 5_000, ..with(vec![branch("pd-a", true)]) });
    assert!(gone.prs.is_empty(), "a branch with no read leaves the carry");
}

#[test_id(t4reips)]
#[test]
fn prs_round_trip_as_rows_keyed_by_bead() {
    let mut r = rig(1, with(vec![with_pr("pd-a", 5, PrStatus::Green)]));
    r.advance(CADENCE_MS).unwrap();
    let rows = kit::assert_kept_as_rows(&r, NAME);
    assert!(rows.iter().any(|k| k == "prs/pd-a"), "{rows:?}");
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}
