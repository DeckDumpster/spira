use super::*;

const H: u64 = 3600;
const NOW: u64 = 10_000_000;

fn hold(kind: Kind, since: Option<u64>, detail: Option<&str>) -> Hold {
    Hold { bead: "sp-x".into(), kind, since, detail: detail.map(String::from), actor: Some("concierge".into()) }
}

fn mail(delivered_at: u64) -> AskMail {
    AskMail { delivered_at, mailbox: "operator".into() }
}

#[test]
fn a_wait_is_lifted_past_its_expiry_and_not_at_or_before_it() {
    let until = NOW - 10;
    let h = hold(Kind::Wait, Some(NOW - 5 * H), Some(&format!("snooze-until:{until}")));
    assert_eq!(decide(NOW, &h, None, None), Some(Action::LiftWait { until }));
    let h = hold(Kind::Wait, Some(NOW - 5 * H), Some(&format!("snooze-until:{NOW}")));
    assert_eq!(decide(NOW, &h, None, None), None, "expiry == now is not yet past");
    let h = hold(Kind::Wait, Some(NOW - 5 * H), Some(&format!("snooze-until:{}", NOW + 1)));
    assert_eq!(decide(NOW, &h, None, None), None);
}

#[test]
fn a_wait_with_no_timed_expiry_is_a_blocker_not_a_snooze() {
    assert_eq!(decide(NOW, &hold(Kind::Wait, Some(0), Some("blocked on sp-y")), None, None), None);
    assert_eq!(decide(NOW, &hold(Kind::Wait, Some(0), None), None, None), None);
}

#[test]
fn a_silent_ask_is_re_raised() {
    let h = hold(Kind::Ask, Some(NOW - 4 * 24 * H), Some("May I?"));
    assert_eq!(decide(NOW, &h, None, None), Some(Action::RaiseAsk), "no delivered question exists");
}

#[test]
fn a_re_raise_is_not_repeated_inside_the_resurface_window() {
    let h = hold(Kind::Ask, Some(0), Some("q"));
    assert_eq!(decide(NOW, &h, None, Some(NOW - H)), None);
    assert_eq!(decide(NOW, &h, None, Some(NOW - ASK_RESURFACE_SECS)), Some(Action::RaiseAsk));
}

#[test]
fn a_delivered_ask_is_resurfaced_once_after_24h_and_not_before() {
    let h = hold(Kind::Ask, Some(0), Some("q"));
    let fresh = mail(NOW - ASK_RESURFACE_SECS + 1);
    assert_eq!(decide(NOW, &h, Some(&fresh), None), None);
    let stale = mail(NOW - ASK_RESURFACE_SECS);
    assert_eq!(
        decide(NOW, &h, Some(&stale), None),
        Some(Action::ResurfaceAsk { delivered_at: stale.delivered_at, mailbox: "operator".into() })
    );
    assert_eq!(decide(NOW, &h, Some(&stale), Some(NOW - H)), None, "already resurfaced for this delivery");
}

#[test]
fn a_poison_is_diagnosed_past_24h_once_and_a_missing_timestamp_is_never_old() {
    let at = |age: u64| hold(Kind::Poison, Some(NOW - age), None);
    assert_eq!(decide(NOW, &at(POISON_DIAGNOSE_SECS - 1), None, None), None);
    assert_eq!(decide(NOW, &at(POISON_DIAGNOSE_SECS), None, None), Some(Action::DiagnosePoison));
    assert_eq!(decide(NOW, &at(POISON_DIAGNOSE_SECS), None, Some(NOW - H)), None, "once per hold");
    assert_eq!(decide(NOW, &hold(Kind::Poison, None, None), None, None), None);
}

#[test]
fn a_manual_hold_is_reminded_past_72h_and_again_every_72h() {
    let at = |age: u64| hold(Kind::Operator, Some(NOW - age), Some("until publishing resumes"));
    assert_eq!(decide(NOW, &at(MANUAL_REMIND_SECS - 1), None, None), None);
    assert_eq!(decide(NOW, &at(MANUAL_REMIND_SECS), None, None), Some(Action::RemindManual));
    assert_eq!(decide(NOW, &at(10 * MANUAL_REMIND_SECS), None, Some(NOW - H)), None);
    assert_eq!(decide(NOW, &at(10 * MANUAL_REMIND_SECS), None, Some(NOW - MANUAL_REMIND_SECS)), Some(Action::RemindManual));
    assert_eq!(decide(NOW, &hold(Kind::Operator, None, None), None, None), None);
}

struct Fake {
    holds: Result<Vec<Hold>, String>,
    asks: Result<BTreeMap<String, AskMail>, String>,
    acts: Vec<(String, &'static str)>,
    fail: bool,
}

impl Fake {
    fn new(holds: Vec<Hold>) -> Fake {
        Fake { holds: Ok(holds), asks: Ok(BTreeMap::new()), acts: Vec::new(), fail: false }
    }
}

impl Env for Fake {
    fn holds(&mut self) -> Result<Vec<Hold>, String> {
        self.holds.clone()
    }
    fn asks(&mut self) -> Result<BTreeMap<String, AskMail>, String> {
        self.asks.clone()
    }
    fn act(&mut self, hold: &Hold, action: &Action) -> Result<(), String> {
        if self.fail {
            return Err("mail down".into());
        }
        self.acts.push((hold.bead.clone(), action.name()));
        Ok(())
    }
}

fn named(bead: &str, mut h: Hold) -> Hold {
    h.bead = bead.into();
    h
}

#[test]
fn the_sweep_visits_every_kind_and_acts_on_the_ones_at_threshold() {
    let mut env = Fake::new(vec![
        named("w-due", hold(Kind::Wait, Some(0), Some(&format!("snooze-until:{}", NOW - 1)))),
        named("w-early", hold(Kind::Wait, Some(0), Some(&format!("snooze-until:{}", NOW + 60)))),
        named("a-silent", hold(Kind::Ask, Some(0), Some("q"))),
        named("p-old", hold(Kind::Poison, Some(NOW - 2 * 24 * H), None)),
        named("p-new", hold(Kind::Poison, Some(NOW - H), None)),
        named("m-old", hold(Kind::Operator, Some(NOW - 4 * 24 * H), Some("why"))),
    ]);
    let mut state = SweepState::default();
    let report = sweep(NOW, &mut env, &mut state, false).unwrap();
    assert_eq!(report.visited, 6);
    let mut acts = env.acts.clone();
    acts.sort();
    assert_eq!(
        acts,
        vec![
            ("a-silent".to_string(), "raise-ask"),
            ("m-old".to_string(), "remind-manual"),
            ("p-old".to_string(), "diagnose-poison"),
            ("w-due".to_string(), "lift-wait"),
        ]
    );
    assert_eq!(state.last_pass, NOW);
}

#[test]
fn a_second_pass_does_not_repeat_what_the_first_did() {
    let mut env = Fake::new(vec![
        named("a-silent", hold(Kind::Ask, Some(0), Some("q"))),
        named("p-old", hold(Kind::Poison, Some(NOW - 2 * 24 * H), None)),
    ]);
    let mut state = SweepState::default();
    sweep(NOW, &mut env, &mut state, false).unwrap();
    env.acts.clear();
    let again = sweep(NOW + 600, &mut env, &mut state, false).unwrap();
    assert!(env.acts.is_empty(), "{:?}", env.acts);
    assert!(again.wanted.is_empty());
}

#[test]
fn a_failed_action_is_retried_and_stays_wanted() {
    let mut env = Fake::new(vec![named("a-silent", hold(Kind::Ask, Some(0), Some("q")))]);
    env.fail = true;
    let mut state = SweepState::default();
    let report = sweep(NOW, &mut env, &mut state, false).unwrap();
    assert!(matches!(report.taken[0].outcome, Outcome::Failed(_)));
    assert!(state.acted.is_empty(), "a failed step is not recorded as taken");
    env.fail = false;
    sweep(NOW + 600, &mut env, &mut state, false).unwrap();
    assert_eq!(env.acts, vec![("a-silent".to_string(), "raise-ask")]);
}

#[test]
fn unreadable_mailboxes_skip_asks_instead_of_raising_every_one() {
    let mut env = Fake::new(vec![
        named("a-1", hold(Kind::Ask, Some(0), Some("q"))),
        named("p-old", hold(Kind::Poison, Some(NOW - 2 * 24 * H), None)),
    ]);
    env.asks = Err("no mailbox".into());
    let mut state = SweepState::default();
    let report = sweep(NOW, &mut env, &mut state, false).unwrap();
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(env.acts, vec![("p-old".to_string(), "diagnose-poison")]);
}

#[test]
fn an_unreadable_store_fails_the_sweep_and_acts_on_nothing() {
    let mut env = Fake::new(vec![]);
    env.holds = Err("store down".into());
    assert!(sweep(NOW, &mut env, &mut SweepState::default(), false).is_err());
    assert!(env.acts.is_empty());
}

#[test]
fn shadow_records_the_step_and_does_none_of_it() {
    let mut env = Fake::new(vec![named("a-silent", hold(Kind::Ask, Some(0), Some("q")))]);
    let mut state = SweepState::default();
    let report = sweep(NOW, &mut env, &mut state, true).unwrap();
    assert_eq!(report.taken[0].outcome, Outcome::Shadowed);
    assert!(env.acts.is_empty() && state.acted.is_empty());
}

#[test]
fn state_for_a_hold_that_is_gone_is_forgotten() {
    let mut state = SweepState::default();
    state.acted.insert("holds:sp-gone:poison".into(), 5);
    sweep(NOW, &mut Fake::new(vec![]), &mut state, false).unwrap();
    assert!(state.acted.is_empty());
}

#[test]
fn state_round_trips_and_a_corrupt_file_is_empty() {
    let dir = testkit::TempDir::new("reconciler-engine-holds-state");
    let path = dir.join("holds.json");
    assert_eq!(load_state(&path), SweepState::default());
    let mut s = SweepState::default();
    s.acted.insert("holds:sp-x:ask".into(), 7);
    s.last_pass = 9;
    save_state(&path, &s).unwrap();
    assert_eq!(load_state(&path), s);
    std::fs::write(&path, "not json").unwrap();
    assert_eq!(load_state(&path), SweepState::default());
}
