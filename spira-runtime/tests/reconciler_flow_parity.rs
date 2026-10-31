//! For one world, `reconciler-flow --pass` and the reconciler-flow handler mail the Concierge the
//! same gaps and reach the same state. The binary is the oracle: it runs unmodified against stubbed
//! `bd`, `spira-lc`, `date`, `mail` and exporter, a real `duckdb` and `tsd-write`, and a fake metrics
//! store; the handler runs on a virtual clock against a fake spira-lc socket serving the same rows.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use reconciler_engine::io::{load_alerted, load_state};
use runtime::host::EffectRunner;
use runtime::ports::VirtualClock;
use runtime::{Engine, Executor, Journal, MemJournal, Outcome};
use spira_runtime::watchers::reconciler_flow::{Carry, STATE};
use spira_runtime::Effects;

mod common;
use common::flow::*;

fn debug_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().to_path_buf()
}

fn on_path(program: &str) -> Option<PathBuf> {
    std::env::var("PATH").ok()?.split(':').map(|d| Path::new(d).join(program)).find(|p| p.is_file())
}

fn shell(path: PathBuf, body: &str) {
    testkit::write_exe(path, &format!("#!/usr/bin/env bash\n{body}\n"));
}

struct Oracle {
    flow: Flow,
    toml: PathBuf,
    seen_mails: usize,
}

impl Oracle {
    fn build(scenario: Scenario, now: u64) -> Oracle {
        let flow = Flow::build("oracle", scenario, now);
        let root = flow.dir.path().display().to_string();
        let ids: Vec<String> = lc_rows(scenario).iter().filter(|r| r.repo == SCOPE).map(|r| format!(r#"{{"id":"{}"}}"#, r.id)).collect();
        fs::write(flow.p("bd-out.json"), format!("[{}]", ids.join(","))).unwrap();
        let all: Vec<String> = lc_rows(scenario).iter().map(|r| format!(r#"{{"bead_id":"{}","state":"{}","repo":"{}"}}"#, r.id, r.state, r.repo)).collect();
        fs::write(flow.p("lc-out.json"), format!("[{}]", all.join(","))).unwrap();
        shell(flow.p("bin/bd"), &format!("case \" $* \" in *' --label {SCOPE} '*) cat {root}/bd-out.json ;; *) echo '[]' ;; esac"));
        shell(flow.p("bin/spira-lc"), &format!("[ \"$1\" = list ] && cat {root}/lc-out.json"));
        shell(flow.p("bin/tsd-lifecycle-export"), "exit 0");
        shell(flow.p("bin/date"), &format!("case \"$*\" in *%s*) cat {root}/now ;; *) cat {root}/now_iso ;; esac"));
        shell(flow.p("bin/mail"), &format!("n=$(ls {root}/mails | wc -l)\n{{ printf '%s\\n' \"$@\"; echo ---; cat; }} > {root}/mails/$((n + 1))"));
        std::os::unix::fs::symlink(on_path("duckdb").unwrap(), flow.p("bin/duckdb")).unwrap();
        std::os::unix::fs::symlink(debug_dir().join("tsd-write"), flow.p("bin/tsd-write")).unwrap();
        let toml_dir = flow.p("toml");
        fs::create_dir_all(&toml_dir).unwrap();
        let declare = [
            ("SPIRA_RUN", flow.run().display().to_string()),
            ("SPIRA_METRICS_ENDPOINT", flow.store.addr().to_string()),
            ("SPIRA_BD", flow.p("bin/bd").display().to_string()),
            ("SPIRA_DB", "unused".into()),
            ("SPIRA_SCOPE_LABEL", SCOPE.into()),
            ("SPIRA_DESIRED_DIR", flow.p("desired").display().to_string()),
            ("SPIRA_FLOW_WINDOW_HOURS", WINDOW_HOURS.to_string()),
            ("SPIRA_FLOW_BASELINE_HOURS", BASELINE_HOURS.to_string()),
            ("SPIRA_FLOW_GRACE_SECS", GRACE_SECS.to_string()),
            ("SPIRA_FLOW_UNOBSERVABLE_GRACE_SECS", UNOBSERVABLE_GRACE_SECS.to_string()),
            ("SPIRA_FLOW_REWORK_WINDOW_HOURS", REWORK_HOURS.to_string()),
            ("SPIRA_FLOW_SENTINEL_PERIOD_SECS", SENTINEL_SECS.to_string()),
        ];
        let declare: Vec<(&str, &str)> = declare.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let toml = spira_config::process::fixture_toml(&toml_dir, &declare);
        Oracle { flow, toml, seen_mails: 0 }
    }

    /// One pass at `now`; the mails it sent, as `Subject\nbody`.
    fn pass(&mut self, now: u64) -> Vec<String> {
        fs::write(self.flow.p("now"), now.to_string()).unwrap();
        fs::write(self.flow.p("now_iso"), tsd::iso_utc(now)).unwrap();
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let path = format!("{}:/usr/bin:/bin", self.flow.p("bin").display());
        let out = Command::new(debug_dir().join("reconciler-flow"))
            .arg("--pass")
            .env_clear()
            .env("PATH", path)
            .env("SPIRA_TOML", &self.toml)
            .env("SPIRA_HOME", home)
            .env("SPIRA_LC_BIN", self.flow.p("bin/spira-lc"))
            .output()
            .unwrap();
        assert!(out.status.success(), "reconciler-flow --pass failed: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        let total = fs::read_dir(self.flow.p("mails")).unwrap().count();
        let mut sent: Vec<String> = (self.seen_mails + 1..=total)
            .map(|n| {
                let text = fs::read_to_string(self.flow.p(&format!("mails/{n}"))).unwrap();
                let (args, body) = text.split_once("\n---\n").unwrap();
                let args: Vec<&str> = args.lines().collect();
                assert_eq!(&args[..2], ["send", "concierge"], "{args:?}");
                let after = |flag: &str| args[args.iter().position(|a| *a == flag).unwrap() + 1];
                assert_eq!((after("--from"), after("--kind")), ("Reconciler <reconciler@spira>", "note"));
                format!("{}\n{}", after("--subject"), body.trim_end())
            })
            .collect();
        self.seen_mails = total;
        sent.sort();
        sent
    }
}

struct Handler {
    flow: Flow,
    engine: Engine,
    journal: Arc<MemJournal>,
    clock: VirtualClock,
    fx: Effects,
}

impl Handler {
    fn build(scenario: Scenario, now: u64) -> Handler {
        let flow = Flow::build("handler", scenario, now);
        let sock = flow.serve_lc(Arc::new(AtomicU64::new(LC_SEQ)));
        let exporter = flow.counting_exporter();
        let fx = Effects { world: flow.world(&sock, &exporter), concierge_mail: Some(Arc::new(spira_runtime::concierge_mail(mail_env(flow.dir.path())))), ..Effects::default() };
        let clock = VirtualClock::new(now * 1000);
        let journal = MemJournal::new();
        let mut engine = Engine::open(journal.clone(), Arc::new(clock.clone()), vec![handler().registration()], 0).unwrap();
        let (id, stream, kind) = spira_runtime::watchers::reconciler_flow::ReconcilerFlow::boot();
        engine.submit(id, stream, kind).unwrap();
        Handler { flow, engine, journal, clock, fx }
    }

    fn pass(&mut self) -> Vec<String> {
        let before = delivered(self.flow.dir.path());
        self.engine.run_until_idle().unwrap();
        loop {
            let pending = self.engine.pending_effects();
            if pending.is_empty() {
                break;
            }
            for p in pending {
                let out = match self.fx.run(&p.key, &p.spec) {
                    Ok(d) => Outcome::Succeeded(d),
                    Err(d) => panic!("effect {} failed: {d}", p.spec.name),
                };
                self.engine.complete(&p.key, out).unwrap();
            }
            self.engine.run_until_idle().unwrap();
        }
        self.clock.advance(INTERVAL_SECS * 1000);
        delivered(self.flow.dir.path()).into_iter().filter(|m| !before.contains(m)).collect()
    }

    fn carry(&self) -> Carry {
        let events = self.journal.read_from(0, 100_000).unwrap();
        let (_, last) = events.iter().rev().find(|(_, e)| e.kind.tag == STATE).expect("a STATE event");
        serde_json::from_str(&last.kind.body).unwrap()
    }
}

fn same(scenario: Scenario, expect: Option<&str>) {
    if on_path("duckdb").is_none() {
        eprintln!("skipped: no duckdb on PATH");
        return;
    }
    assert!(debug_dir().join("reconciler-flow").exists() && debug_dir().join("tsd-write").exists(), "build reconciler-flow and tsd-write first: they are the oracle");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let mut oracle = Oracle::build(scenario, now);
    let mut handler = Handler::build(scenario, now);
    let mut every_mail = vec![];
    for pass in 0..3 {
        let at = now + pass * INTERVAL_SECS;
        let (old, new) = (oracle.pass(at), handler.pass());
        assert_eq!(new, old, "pass {pass}: the handler and the binary mail different things");
        let carry = handler.carry();
        assert_eq!(carry.state, load_state(&oracle.flow.state_file()), "pass {pass}: hysteresis state");
        assert_eq!(carry.alerted, load_alerted(&oracle.flow.alerted_file()), "pass {pass}: alerted streaks");
        assert_eq!(handler.flow.statuses(), oracle.flow.statuses(), "pass {pass}: recorded statuses");
        every_mail.extend(old);
    }
    match expect {
        Some(key) => {
            let subject = format!("RECONCILER: flow gap — {key}");
            assert_eq!(every_mail.iter().filter(|m| m.starts_with(&subject)).count(), 1, "{key} is mailed once per streak: {every_mail:?}");
        }
        None => assert!(every_mail.is_empty(), "a quiet world mails nothing: {every_mail:?}"),
    }
    assert_eq!(handler.flow.exports(), 0, "a lifecycle log the export has caught up with is not exported again");
}

#[test]
fn a_backlog_rising_past_its_baseline_is_mailed_the_same() {
    same(Scenario::BacklogRising, Some("flow:backlog"));
}

#[test]
fn a_stalled_stage_is_mailed_the_same() {
    same(Scenario::StalledStage, Some("flow:velocity:queue"));
}

#[test]
fn a_dwell_regression_is_mailed_the_same() {
    same(Scenario::DwellRegression, Some("flow:dwell-regression:working"));
}

#[test]
fn a_high_flip_rate_is_mailed_the_same() {
    same(Scenario::HighFlipRate, Some("flow:round-health"));
}

#[test]
fn an_all_quiet_world_is_mailed_nothing_by_either() {
    same(Scenario::Quiet, None);
}
