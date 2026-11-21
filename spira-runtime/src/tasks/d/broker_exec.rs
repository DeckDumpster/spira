//! # broker.exec — contract
//!
//! Runs the `work` verbs that start a program. lc-serve records each as a request in the control
//! store (`work-request`) and answers at once with its id; this task takes the request, runs
//! `spira-lc work-exec <id>` in its own process under `timeout`, and deals with the fallout:
//! a runner that dies, times out or is lost to a restart leaves the request finished with exit
//! 124 and the stage it reached. Pattern P1.
//!
//! ## Tick
//!
//! - `Timer` every 1 s; single-flight.
//!
//! ## Observe
//!
//! `broker.exec.observe` returns `Snapshot { queued, stale }`: the queued requests, and the running
//! ones whose bound and a grace have passed with no answer recorded (the scheduler restarted while
//! their runner was alive, or the runner was lost). Each entry carries its id and its verb's bound.
//!
//! ## Decide
//!
//! One effect per queued request and one per stale one. No carry.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `broker.exec.observe` (probe) | store | 10 s | none | |
//! | `broker.exec.run` | broker | the verb's bound plus 20 s | `broker.exec:<request id>` (`:stale` for a stale one) | a request that is not queued: another runner has it |
//!
//! - **`broker.exec.run`** takes the request (`work-request take`, queued to running, refused when
//!   not queued), runs `timeout -k 5 <bound> spira-lc work-exec <id>`, then reads the record. A runner
//!   that recorded its answer is done. One that did not (killed, crashed, cut off at its bound) has
//!   the request finished with exit 124 and a reason naming how the runner ended. The effect runs
//!   under `Effects::once` on the request id, so a restart does not run a request twice; a request
//!   already taken is refused by the store whether or not the ledger survived.
//!
//! ## Silence
//!
//! An idle tick emits the probe only.
//!
//! ## Reverse
//!
//! Observe mode shadows `run`: requests stay queued and the `work` clients wait out their bound.

use std::time::Duration;

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_store::lc_call::LcClient;

use super::pattern;

pub const NAME: &str = "broker.exec";
pub const STREAM: &str = "broker.exec";
pub const TICK: EventKindTag = "broker.exec.tick";
pub const STATE: EventKindTag = "broker.exec.state";
pub const OBSERVE: &str = "broker.exec.observe";
pub const RUN: &str = "broker.exec.run";
pub const EFFECTS: &[&str] = &[OBSERVE, RUN];
pub const KEYS: &[&str] = &[];
pub const CADENCE_MS: u64 = 1_000;
const OBSERVE_DEADLINE_MS: u64 = 10_000;
/// A running request is stale this long after its bound, so a live runner is never taken for lost.
const STALE_GRACE_MS: u64 = 60_000;
/// What the effect's deadline allows beyond the verb's bound: the kill grace and the final read.
const MARGIN_MS: u64 = 20_000;
const REASON_TAIL: usize = 1_500;
const STORE_WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub bound_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub queued: Vec<Entry>,
    pub stale: Vec<Entry>,
}

/// What `run` takes: one request, and whether it is a stale running one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    pub id: String,
    pub bound_ms: u64,
    pub stale: bool,
}

/// One effect per queued request and per stale one, each keyed on the request id.
pub fn decide(snap: &Snapshot) -> Vec<EffectSpec> {
    let job = |e: &Entry, stale: bool| {
        let key = if stale { format!("{NAME}:{}:stale", e.id) } else { format!("{NAME}:{}", e.id) };
        let input = serde_json::to_string(&Job { id: e.id.clone(), bound_ms: e.bound_ms, stale }).unwrap_or_default();
        EffectSpec::keyed(RUN, key, input, if stale { MARGIN_MS } else { e.bound_ms + MARGIN_MS })
    };
    snap.queued.iter().map(|e| job(e, false)).chain(snap.stale.iter().map(|e| job(e, true))).collect()
}

fn entries(raw: &str) -> Result<Vec<serde_json::Value>, String> {
    serde_json::from_str(raw).map_err(|e| format!("work-request list: {e}"))
}

/// The probe: the queued requests and the running ones past their bound.
pub fn observe(e: &crate::Effects, _key: &str, _spec: &EffectSpec) -> Result<String, String> {
    let lc = LcClient::socket(&e.world.lc_socket).within(STORE_WAIT);
    let listed = |state: &str| lc.answer(&["work-request", "list", "--state", state]).map_err(|e| e.to_string()).and_then(|raw| entries(&raw));
    let entry = |r: &serde_json::Value| Entry { id: r["id"].as_str().unwrap_or("").to_string(), bound_ms: r["bound_ms"].as_u64().unwrap_or(0) };
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let queued = listed("queued")?.iter().map(entry).collect();
    let stale = listed("running")?
        .iter()
        .filter(|r| r["started_at"].as_u64().unwrap_or(0).saturating_mul(1000).saturating_add(r["bound_ms"].as_u64().unwrap_or(0)).saturating_add(STALE_GRACE_MS) < now_ms)
        .map(entry)
        .collect();
    serde_json::to_string(&Snapshot { queued, stale }).map_err(|e| e.to_string())
}

/// Takes the request and runs its `work-exec`; see the contract.
pub fn run(e: &crate::Effects, key: &str, spec: &EffectSpec) -> Result<String, String> {
    let job: Job = serde_json::from_str(&spec.input).map_err(|e| format!("{RUN}: bad input: {e}"))?;
    let lc = LcClient::socket(&e.world.lc_socket).within(STORE_WAIT);
    e.once(key, || run_job(&lc, &job, &crate::broker_host::exec_program))
}

/// How the runner ended: its exit code and what it printed. 124 is the bound.
type Exec<'a> = &'a dyn Fn(&str, u64) -> (i32, String);

fn run_job(lc: &LcClient, job: &Job, exec: Exec) -> Result<String, String> {
    if job.stale {
        let reason = "the scheduler lost the runner while the request was running (stage: running); nothing recorded its answer";
        return finish(lc, &job.id, reason).map(|()| format!("{}: finished stale", job.id));
    }
    match lc.call(&["work-request", "take", &job.id]) {
        Ok(r) if r.code == 0 => {}
        Ok(r) if r.code == 3 => return Ok(format!("{}: not queued, left to its runner", job.id)),
        Ok(r) => return Err(format!("{}: take: {}", job.id, r.text())),
        Err(e) => return Err(format!("{}: take: {e}", job.id)),
    }
    let (code, said) = exec(&job.id, job.bound_ms);
    if recorded(lc, &job.id)? {
        return Ok(format!("{}: ran, exit {code}", job.id));
    }
    let how = if code == 124 { "cut off at its bound".to_string() } else { format!("ended with exit {code} (a signal reads as 2) before recording an answer") };
    let tail: String = said.chars().rev().take(REASON_TAIL).collect::<Vec<_>>().into_iter().rev().collect();
    let reason = format!("work-exec {how} (stage: running); {}", if tail.trim().is_empty() { "it printed nothing".to_string() } else { format!("it printed:\n{}", tail.trim_end()) });
    finish(lc, &job.id, &reason).map(|()| format!("{}: finished 124, runner {how}", job.id))
}

/// Whether the request has its answer: a runner that wrote it is never overwritten.
fn recorded(lc: &LcClient, id: &str) -> Result<bool, String> {
    let shown = lc.answer(&["work-request", "show", id]).map_err(|e| format!("{id}: show: {e}"))?;
    let record: serde_json::Value = serde_json::from_str(&shown).map_err(|e| format!("{id}: show: {e}"))?;
    Ok(record["state"] == "done")
}

/// Finishes the request with exit 124 and `reason`; a request already finished is left as it is.
fn finish(lc: &LcClient, id: &str, reason: &str) -> Result<(), String> {
    match lc.call_stdin(&["work-request", "finish", id, "--code", "124"], reason) {
        Ok(r) if r.code == 0 || r.code == 3 => Ok(()),
        Ok(r) => Err(format!("{id}: finish: {}", r.text())),
        Err(e) => Err(format!("{id}: finish: {e}")),
    }
}

pub struct BrokerExec;

impl BrokerExec {
    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("broker.exec/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for BrokerExec {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &Ok::<(), String>(()), |_: &(), snap: &Snapshot, _, _: &()| ((), decide(snap)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn entry(id: &str) -> Entry {
        Entry { id: id.into(), bound_ms: 300_000 }
    }

    #[test_id(t4d3gdy)]
    #[test]
    fn each_queued_request_is_one_effect_keyed_on_its_id_with_the_verbs_bound() {
        let specs = decide(&Snapshot { queued: vec![entry("wr-1"), entry("wr-2")], stale: vec![] });
        assert_eq!(specs.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(), ["broker.exec:wr-1", "broker.exec:wr-2"]);
        assert!(specs.iter().all(|s| s.name == RUN && s.deadline_ms == 300_000 + MARGIN_MS));
    }

    #[test_id(t6a17xw)]
    #[test]
    fn a_stale_running_request_gets_its_own_key_so_it_never_shares_the_queued_ones_ledger() {
        let specs = decide(&Snapshot { queued: vec![], stale: vec![entry("wr-3")] });
        assert_eq!(specs[0].key, "broker.exec:wr-3:stale");
        assert!(serde_json::from_str::<Job>(&specs[0].input).unwrap().stale);
    }

    #[test_id(tcud1d9)]
    #[test]
    fn nothing_queued_decides_nothing_and_a_quiet_answer_reads_as_nothing_queued() {
        assert!(decide(&Snapshot::default()).is_empty());
        assert_eq!(serde_json::from_str::<Snapshot>("{}").unwrap(), Snapshot::default());
    }

    struct Lc {
        _dir: testkit::TempDir,
        client: LcClient,
        seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    }

    /// A store that answers `show` with `state`, takes with `take_code`, and records every request.
    fn lc(state: &'static str, take_code: i32) -> Lc {
        use std::io::{BufRead, BufReader, Write};
        let dir = testkit::TempDir::new("broker-exec-lc");
        let path = dir.join("sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
                let mut argv: Vec<String> = value.get("argv").unwrap_or(&value).as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_string()).collect();
                if let Some(len) = value.get("len").and_then(|l| l.as_u64()) {
                    let mut body = vec![0u8; len as usize];
                    std::io::Read::read_exact(&mut reader, &mut body).unwrap();
                    argv.push(format!("body:{}", String::from_utf8_lossy(&body)));
                }
                let (code, out) = match argv.get(1).map(String::as_str) {
                    Some("take") => (take_code, String::new()),
                    Some("show") => (0, format!("{{\"state\":\"{state}\"}}")),
                    _ => (0, String::new()),
                };
                log.lock().unwrap().push(argv);
                let mut w = &stream;
                let _ = write!(w, "{{\"exit_code\":{code},\"len\":{}}}\n{out}", out.len());
            }
        });
        Lc { _dir: dir, client: LcClient::socket(path), seen }
    }

    fn finished(lc: &Lc) -> Vec<Vec<String>> {
        lc.seen.lock().unwrap().iter().filter(|a| a.get(1).map(String::as_str) == Some("finish")).cloned().collect()
    }

    fn job() -> Job {
        Job { id: "wr-9".into(), bound_ms: 300_000, stale: false }
    }

    #[test_id(tzlbokm)]
    #[test]
    fn a_runner_that_recorded_its_answer_is_not_finished_again() {
        let lc = lc("done", 0);
        let ran = RefCell::new(0);
        let out = run_job(&lc.client, &job(), &|_, _| {
            *ran.borrow_mut() += 1;
            (0, String::new())
        })
        .unwrap();
        assert_eq!((*ran.borrow(), finished(&lc).len()), (1, 0), "{out}");
    }

    #[test_id(t6jui3n)]
    #[test]
    fn a_runner_that_died_or_hit_its_bound_leaves_the_request_finished_124_with_a_reason() {
        for (code, said, want) in [(124, "", "cut off at its bound"), (137, "killed", "ended with exit 137")] {
            let lc = lc("running", 0);
            run_job(&lc.client, &job(), &|_, _| (code, said.to_string())).unwrap();
            let f = finished(&lc);
            assert_eq!(f.len(), 1, "{code}: finished once");
            assert!(f[0].contains(&"124".to_string()), "{:?}", f[0]);
            let stdin = f[0].last().cloned().unwrap_or_default();
            assert!(stdin.contains(want) && stdin.contains("stage: running"), "{stdin}");
        }
    }

    #[test_id(tp7x8uz)]
    #[test]
    fn a_request_another_runner_took_is_left_alone_and_never_run() {
        let lc = lc("running", 3);
        let out = run_job(&lc.client, &job(), &|_, _| panic!("a refused take must not run the program")).unwrap();
        assert!(out.contains("not queued"), "{out}");
        assert!(finished(&lc).is_empty());
    }

    #[test_id(tm7d4va)]
    #[test]
    fn a_stale_request_is_finished_without_taking_or_running_anything() {
        let lc = lc("running", 0);
        run_job(&lc.client, &Job { stale: true, ..job() }, &|_, _| panic!("a stale request runs nothing")).unwrap();
        assert_eq!(finished(&lc).len(), 1);
        assert!(lc.seen.lock().unwrap().iter().all(|a| a.get(1).map(String::as_str) != Some("take")));
    }
}
