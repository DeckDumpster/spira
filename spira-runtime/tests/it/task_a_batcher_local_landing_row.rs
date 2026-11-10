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
    ran: Mutex<Vec<String>>,
}

impl EffectRunner for Runner {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        self.ran.lock().unwrap().push(spec.name.clone());
        match spec.name.as_str() {
            OBSERVE => Ok(serde_json::to_string(&waiting_pool(&self.settled, self.clock.now_ms())).unwrap()),
            FETCH | CERTIFY => Ok("{}".into()),
            ASSEMBLE => Ok(serde_json::to_string(&assembled()).unwrap()),
            ROUND_OPEN => Ok(serde_json::to_string(&Opened { batch: "next-round".into(), ..Opened::default() }).unwrap()),
            other => Err(format!("no double for {other}")),
        }
    }
}

fn waiting_pool(settled: &str, now_ms: u64) -> Snapshot {
    let member = Member { id: "sp-ypesca".into(), state: "CERTIFIED".into(), version: 5, tip: "t1".into(), sifted_tip: Some("t1".into()), title: "work".into(), priority: 2, ..Member::default() };
    let repo = Repo { branch_tips: [("sp-ypesca".to_string(), "t1".to_string())].into(), pool: vec![member], base_sha: "b2".into(), settled: vec![settled.into()], live_legs: Some(vec![]), ..Repo::default() };
    Snapshot { repos: [("spira".to_string(), repo)].into(), now_ms }
}

fn assembled() -> Assembled {
    Assembled { merged: vec![("sp-ypesca".into(), "t1".into())], wt: "/w".into(), base: "b2".into(), own_testenv: true, ..Assembled::default() }
}

fn tick_on_row(row: &str, settled: &str) -> Vec<String> {
    let journal = MemJournal::new();
    journal.commit(&Turn { state: vec![StateRow { task: "batcher.local".into(), skey: "repos/spira".into(), value: format!(r#"{{"key":"spira","value":{row}}}"#), at: NOW }], ..Turn::default() }).unwrap();
    let clock = VirtualClock::new(NOW);
    let runner = Arc::new(Runner { clock: clock.clone(), settled: settled.into(), ran: Mutex::new(vec![]) });
    let mut engine = open_engine(&fixture(), journal.clone(), Arc::new(clock.clone()), Arc::new(NullMarker), Arc::new(NullSink), vec![]).unwrap();
    start_tasks(&mut engine);
    let mut d = VirtualDriver::new(engine, clock, runner.clone());
    d.run_until(NOW + 180_000).unwrap();
    let ran = runner.ran.lock().unwrap().iter().filter(|n| n.starts_with("batcher.local")).cloned().collect();
    ran
}

#[test]
fn a_tick_on_a_stored_landing_row_for_a_landed_batch_cuts_the_waiting_member() {
    let ran = tick_on_row(STORED, BATCH);
    assert!(ran.iter().any(|n| n == ROUND_OPEN), "the landed round was forgotten and the waiting member's round opened: {ran:?}");
}

#[test]
fn a_tick_on_a_stored_landing_row_for_a_batch_the_lifecycle_has_not_settled_waits() {
    let ran = tick_on_row(STORED, "some-other-batch");
    assert!(!ran.iter().any(|n| n == ROUND_OPEN), "{ran:?}");
}

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
