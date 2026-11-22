//! A Landing row whose batch the lifecycle reports LANDED, left behind because the landing's own
//! outcome died with the process the landing restarted, is dropped and the next round is cut.

use std::sync::{Arc, Mutex};

use runtime::host::{EffectRunner, VirtualDriver};
use runtime::journal::{Journal, StateRow, Turn};
use runtime::ports::{Clock, VirtualClock};
use runtime::units::{Exit, UnitHandle, UnitRan};
use runtime::{EffectSpec, MemJournal, NullMarker, NullSink};
use spira_runtime::settings::fixture;
use spira_runtime::tasks::a::batcher_local::*;
use spira_runtime::{open_engine, start_tasks};

const NOW: u64 = 1_794_349_965_000 + 700_000;
const BATCH: &str = "spira-20261110T220627Z";
const STORED: &str = r#"{"state":"Landing","batch":"spira-20261110T220627Z","leg":"spira.172b487182ed.certify1","leg_at_ms":1794349438717,"run":1,"leg_restarts":0,"leg_missing":0,"paused_on":null,"skip_why":null,"waiting":{"state":"Landing","since_ms":1794349965000},"stacked":false,"attributed":false,"round_key":"sp-tzby5y:aaaaaaa sp-226dns.2:bbbbbbb"}"#;

struct Runner {
    clock: VirtualClock,
    settled: String,
    landed: bool,
    ran: Mutex<Vec<String>>,
    supersede: Mutex<Vec<String>>,
}

impl EffectRunner for Runner {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        self.ran.lock().unwrap().push(spec.name.clone());
        if spec.name == SUPERSEDE_REDS {
            self.supersede.lock().unwrap().push(spec.input.clone());
        }
        match spec.name.as_str() {
            OBSERVE => Ok(observed(&self.settled, self.landed, self.clock.now_ms()).to_string()),
            FETCH | CERTIFY | LAND | SUPERSEDE_REDS => Ok("{}".into()),
            ASSEMBLE => Ok(serde_json::to_string(&assembled()).unwrap()),
            ROUND_OPEN => Ok(serde_json::to_string(&Opened { batch: "next-round".into(), ..Opened::default() }).unwrap()),
            other => Err(format!("no double for {other}")),
        }
    }
}

fn waiting_pool(settled: &str, now_ms: u64) -> Snapshot {
    let member = Member { id: "sp-ypesca".into(), state: "CERTIFIED".into(), version: 5, tip: "t1".into(), sifted_tip: Some("t1".into()), title: "work".into(), priority: 2, ..Member::default() };
    let repo = Repo { branch_tips: [("sp-ypesca".to_string(), "t1".to_string())].into(), pool: vec![member], base_sha: "b2".into(), settled: vec![settled.into()], live_legs: Some(vec![]), ..Repo::default() };
    Snapshot { repos: [("spira".to_string(), repo)].into(), now_ms, ..Default::default() }
}

fn observed(settled: &str, landed: bool, now_ms: u64) -> serde_json::Value {
    let mut snap = serde_json::to_value(waiting_pool(settled, now_ms)).unwrap();
    if landed {
        snap["repos"]["spira"]["landed"] = serde_json::json!([settled]);
    }
    snap
}

fn assembled() -> Assembled {
    Assembled { merged: vec![("sp-ypesca".into(), "t1".into())], wt: "w".into(), base: "b2".into(), own_testenv: true, ..Assembled::default() }
}

fn tick_on_row(row: &str, settled: &str) -> Vec<String> {
    tick_on_row_landed(row, settled, false).0
}

fn tick_on_row_landed(row: &str, settled: &str, landed: bool) -> (Vec<String>, Vec<String>) {
    let journal = MemJournal::new();
    journal.commit(&Turn { state: vec![StateRow { task: "batcher.local".into(), skey: "repos/spira".into(), value: format!(r#"{{"key":"spira","value":{row}}}"#), at: NOW }], ..Turn::default() }).unwrap();
    let clock = VirtualClock::new(NOW);
    let runner = Arc::new(Runner { clock: clock.clone(), settled: settled.into(), landed, ran: Mutex::new(vec![]), supersede: Mutex::new(vec![]) });
    let mut engine = open_engine(&fixture(), journal.clone(), Arc::new(clock.clone()), Arc::new(NullMarker), Arc::new(NullSink), vec![], std::time::Duration::from_secs(10)).unwrap();
    start_tasks(&mut engine);
    let mut d = VirtualDriver::new(engine, clock, runner.clone());
    d.run_until(NOW + 180_000).unwrap();
    let ran = runner.ran.lock().unwrap().iter().filter(|n| n.starts_with("batcher.local")).cloned().collect();
    let supersede = runner.supersede.lock().unwrap().clone();
    (ran, supersede)
}

#[test_id(turag6c)]
#[test]
fn a_tick_on_a_stored_landing_row_for_a_landed_batch_cuts_the_waiting_member() {
    let ran = tick_on_row(STORED, BATCH);
    assert!(ran.iter().any(|n| n == ROUND_OPEN), "the landed round was forgotten and the waiting member's round opened: {ran:?}");
}

#[test_id(t6od8gj)]
#[test]
fn a_tick_on_a_stored_landing_row_for_a_batch_the_lifecycle_has_not_settled_lands_it_before_cutting_another() {
    let ran = tick_on_row(STORED, "some-other-batch");
    let land = ran.iter().position(|n| n == LAND).unwrap_or_else(|| panic!("the unsettled round was not landed again: {ran:?}"));
    assert!(ran.iter().position(|n| n == ROUND_OPEN).is_none_or(|open| open > land), "{ran:?}");
}

#[test_id(t3ofc5a)]
#[test]
fn a_second_round_is_cut_after_the_first_lands_and_the_process_is_replaced() {
    let now = NOW;
    let snap = waiting_pool("", now);
    let (carry, acts) = decide(&Carry::default(), &snap);
    assert!(acts.iter().any(|a| a.effect == ASSEMBLE), "{acts:?}");
    let leg_of = |acts: &[spira_runtime::tasks::a::kit::Act], effect: &str| -> Leg { serde_json::from_str(&acts.iter().find(|a| a.effect == effect).unwrap().input).unwrap() };
    let (carry, acts) = on_outcome(&carry, ASSEMBLE, true, &serde_json::to_string(&assembled()).unwrap(), &leg_of(&acts, ASSEMBLE), now);
    let (carry, acts) = on_outcome(&carry, ROUND_OPEN, true, &serde_json::to_string(&Opened { batch: "round-one".into(), ..Opened::default() }).unwrap(), &leg_of(&acts, ROUND_OPEN), now);
    let certify = leg_of(&acts, CERTIFY);
    let ran = UnitRan { handle: UnitHandle::of(NAME, &certify.unit).unwrap(), exit: Exit::Code(0), duration_ms: 1 };
    let (carry, acts) = on_ran(&carry, &ran, now).expect("the certify leg's end reaches the round");
    assert!(acts.iter().any(|a| a.effect == LAND), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Landing);

    // The landing activates a release, the process is replaced, and the land outcome is never delivered.
    let row = serde_json::to_string(&carry.repos["spira"]).unwrap();
    let ran = tick_on_row(&row, "round-one");
    assert!(ran.iter().any(|n| n == ROUND_OPEN), "the second round was cut with no outside event: {ran:?}");
}

#[test_id(tb6sj58)]
#[test]
fn every_lifecycle_driven_state_drops_a_row_whose_batch_is_landed() {
    for state in ["Mending", "Certified", "Corpus", "Attributing", "Certifying"] {
        let row = STORED.replace("\"state\":\"Landing\"", &format!("\"state\":\"{state}\"")).replace("\"waiting\":{\"state\":\"Landing\",\"since_ms\":1794349965000}", "\"waiting\":null");
        let rc: RepoCarry = serde_json::from_str(&row).unwrap();
        let carry = Carry { repos: [("spira".to_string(), rc)].into() };
        let (_, acts) = decide(&carry, &waiting_pool(BATCH, NOW));
        assert!(acts.iter().any(|a| a.effect == ASSEMBLE), "{state}: the settled round was forgotten and the next cut: {acts:?}");
    }
}

fn land_acts_for_landing_row(in_flight: Option<Vec<String>>) -> Vec<String> {
    let row = STORED.replace("\"waiting\":{\"state\":\"Landing\",\"since_ms\":1794349965000}", "\"waiting\":null");
    assert_ne!(row, STORED, "the fixture still carries its waiting");
    let rc: RepoCarry = serde_json::from_str(&row).unwrap();
    let carry = Carry { repos: [("spira".to_string(), rc)].into() };
    let mut snap = waiting_pool("some-other-batch", NOW);
    snap.repos.get_mut("spira").unwrap().in_flight = in_flight;
    let (_, acts) = decide(&carry, &snap);
    acts.iter().map(|a| a.effect.clone()).collect()
}

#[test_id(tprnegx)]
#[test]
fn a_landing_round_asks_to_land_again_only_when_the_scheduler_holds_other_effects_in_flight_and_no_land() {
    let asked = land_acts_for_landing_row(Some(vec![]));
    assert!(asked.iter().any(|e| e == LAND), "{asked:?}");
}

#[test_id(t8ncdrp)]
#[test]
fn a_landing_round_does_not_ask_again_while_its_land_is_in_flight() {
    let asked = land_acts_for_landing_row(Some(vec![LAND.to_string()]));
    assert!(!asked.iter().any(|e| e == LAND), "{asked:?}");
}

#[test_id(t12j019)]
#[test]
fn a_landing_round_does_not_ask_again_when_the_scheduler_reports_no_in_flight_view() {
    let asked = land_acts_for_landing_row(None);
    assert!(!asked.iter().any(|e| e == LAND), "{asked:?}");
}

const THREE_MEMBERS: &str = r#","members":[["sp-o82moc","t1"],["sp-msth37","t2"],["sp-vtnwny","t3"]]"#;

fn three_member_row() -> String {
    let row = STORED.replacen(r#","run":1"#, &format!(r#"{THREE_MEMBERS},"run":1"#), 1);
    assert_ne!(row, STORED);
    row
}

#[test]
fn a_landing_row_dropped_because_its_batch_is_landed_closes_the_reds_for_every_member() {
    let (ran, supersede) = tick_on_row_landed(&three_member_row(), BATCH, true);
    assert_eq!(ran.iter().filter(|n| *n == SUPERSEDE_REDS).count(), 1, "{ran:?}");
    let leg: Leg = serde_json::from_str(&supersede[0]).unwrap();
    let ids: Vec<&str> = leg.members.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["sp-o82moc", "sp-msth37", "sp-vtnwny"]);
}

#[test]
fn a_landing_row_dropped_because_its_batch_is_abandoned_closes_no_reds() {
    let (ran, _) = tick_on_row_landed(&three_member_row(), BATCH, false);
    assert!(!ran.iter().any(|n| n == SUPERSEDE_REDS), "{ran:?}");
}
