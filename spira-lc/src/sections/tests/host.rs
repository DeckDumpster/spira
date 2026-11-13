//! The Host section end to end: `watchtower::host_section::run` against the control store's
//! in-memory backend through the verbs a deployed watchtower pass calls, on the rig's virtual
//! clock, with `certify_par` held by a fake `spira-config` and the reaps by a fake `target-reap`.

use std::cell::{Cell, RefCell};

use reconciler_engine::goodput_pass::Lc;
use spira_desired_state::sections::host;
use spira_desired_state::store::format_rfc3339;
use watchtower::fsstat::Usage;
use watchtower::host_section::{self as hs, Cfg, Config, Ports, Reaper, Report, Sample, Stale};

use super::*;

struct Dispatch<'a, B: Backend>(&'a Rig<B>);

impl<B: Backend> Lc for Dispatch<'_, B> {
    fn call(&self, args: &[String]) -> (i32, Value) {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        self.0.call(&argv, None)
    }
}

struct Par {
    par: Cell<u64>,
    sets: RefCell<Vec<u64>>,
}

impl Config for Par {
    fn get(&self, key: &str) -> Result<u64, String> {
        assert_eq!(key, host::CERTIFY_PAR_KEY);
        Ok(self.par.get())
    }

    fn set(&self, key: &str, value: u64) -> Result<(), String> {
        assert_eq!(key, host::CERTIFY_PAR_KEY, "only certify_par is ever set");
        self.sets.borrow_mut().push(value);
        self.par.set(value);
        Ok(())
    }
}

struct Stales {
    scratch: Cell<u64>,
    targets: Cell<u64>,
    reaps: RefCell<Vec<(Stale, f64, u64)>>,
}

impl Stales {
    fn cell(&self, what: Stale) -> &Cell<u64> {
        match what {
            Stale::Scratch => &self.scratch,
            Stale::Targets => &self.targets,
        }
    }
}

impl Reaper for Stales {
    fn stale(&self, what: Stale) -> Result<u64, String> {
        Ok(self.cell(what).get())
    }

    fn reap(&self, what: Stale, fraction: f64) -> Result<u64, String> {
        let n = self.cell(what).get();
        let gone = ((n as f64) * fraction - 1e-9).ceil() as u64;
        self.cell(what).set(n - gone);
        self.reaps.borrow_mut().push((what, fraction, gone));
        Ok(gone)
    }
}

fn secs(r: &Rig<MemBackend>) -> u64 {
    r.clock.now_ms() / 1000
}

fn cfg() -> Cfg {
    Cfg { scratch: "/scratch".into(), worktrees: "/worktrees".into(), root: "/".into(), psi_dir: "/psi".into() }
}

/// The 2026-10-31 host samples: scratch 15% free, the worktree disk 94% full, root 40% free, and
/// the io pressure file as the kernel writes it.
fn day_1031() -> Sample {
    let usage = |p: &str| {
        let (avail_mib, size_mib) = match p {
            "/scratch" => (1500, 10_000),
            "/worktrees" => (600, 10_000),
            "/" => (4_000, 10_000),
            _ => return None,
        };
        Some(Usage { avail_mib, size_mib, used_pct: 100 - avail_mib * 100 / size_mib })
    };
    let read = |p: &str| (p == "/psi/io").then(|| "some avg10=60.00 avg60=70.00 avg300=55.00 total=1\nfull avg10=70.00 avg60=77.00 avg300=52.00 total=2\n".to_string());
    hs::sample(&cfg(), &usage, &read)
}

fn calm() -> Sample {
    Sample { scratch: Some(0.9), worktrees: Some(0.9), root: Some(0.9), io: Some(3.0) }
}

struct World {
    r: Rig<MemBackend>,
    par: Par,
    stale: Stales,
    steps: RefCell<Vec<String>>,
    token: RefCell<Option<String>>,
}

fn world(par: u64, scratch: u64, targets: u64) -> World {
    let r = rig();
    let fragment = parse_fragment(include_str!("../../../../desired-state/examples/host.toml")).unwrap();
    for s in spira_desired_state::compose::section_specs(&compose(&[fragment]).unwrap()) {
        r.apply_spec(&s);
    }
    World {
        r,
        par: Par { par: Cell::new(par), sets: RefCell::new(vec![]) },
        stale: Stales { scratch: Cell::new(scratch), targets: Cell::new(targets), reaps: RefCell::new(vec![]) },
        steps: RefCell::new(vec![]),
        token: RefCell::new(None),
    }
}

impl World {
    fn run(&self, sample: &Sample) -> Result<Report, String> {
        let d = Dispatch(&self.r);
        let record = |l: &str| self.steps.borrow_mut().push(l.to_string());
        hs::run(&Ports { lc: &d, config: &self.par, reaper: &self.stale, record: &record }, secs(&self.r), sample)
    }

    fn section(&self) -> Value {
        self.r.ok(&["section", "get", "Host/host"], None)["section"].clone()
    }

    fn put(&self, id: &str, action: &str, from: u64, to: u64) -> String {
        let token = self.token.borrow_mut().get_or_insert_with(|| self.r.lock()).clone();
        let key = format!("intents/Host/host/{id}");
        let intent = json!({
            "id": id, "action": action, "target": format!("knobs/Host/host/{action}"), "from": from, "to": to,
            "moves": "io_full_avg300", "baseline": 52.0, "expected_effect": -5.0, "horizon": "15m",
            "addresses": ["IoPressure"], "taken_at": format_rfc3339(secs(&self.r)), "by": { "pass": "p-1", "lock_token": token },
        });
        self.r.ok(&["intent", "put", &key, "--token", &token, "--actor", "reconciler"], Some(&intent));
        key
    }

    fn ack(&self, key: &str) -> Value {
        self.r.ok(&["intent", "get", key], None)["intent"]["ack"].clone()
    }
}

fn condition(s: &Value, type_: &str) -> String {
    s["status"]["conditions"].as_array().unwrap().iter().find(|c| c["type"] == type_).unwrap()["status"].as_str().unwrap().to_string()
}

#[test]
fn the_1031_samples_read_as_the_manifest_levels_and_distance_and_no_24h_field_enters_it() {
    let w = world(2, 20, 20);
    assert!(w.run(&day_1031()).unwrap().published);
    let s = w.section();
    let st = &s["status"];
    assert_eq!(st[host::SCRATCH], json!({ "value": 0.15, "window": "level" }));
    assert_eq!(st[host::WORKTREES], json!({ "value": 0.06, "window": "level" }));
    assert_eq!(st[host::ROOT], json!({ "value": 0.4, "window": "level" }));
    assert_eq!(st[host::IO], json!({ "value": 52.0, "window": "5m" }));
    assert_eq!(st[host::CERTIFY_PAR], json!({ "value": 2.0, "window": "level" }));
    assert!(st.get(host::SHED_FROM).is_none());

    let parsed = spira_desired_state::section::parse_section(&s).unwrap();
    let kind = spira_desired_state::sections::lookup(host::NAME).unwrap();
    // scratch (0.30-0.15)/0.30 + worktrees (0.20-0.06)/0.20 + io (52-40)/40 = 0.5 + 0.7 + 0.3.
    assert!((parsed.distance_value(kind).unwrap().unwrap() - 1.5).abs() < 1e-9);
    let spec = parsed.spec.clone();
    let targets = hs::targets_of(Some(&parsed)).expect("the shipped spec is seeded");
    assert_eq!(spec.len(), 3);
    assert!((hs::distance(&day_1031(), &targets).unwrap() - 1.5).abs() < 1e-9, "the status core and the kind's expression agree");
    for word in host::DISTANCE.split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')).filter(|w| w.chars().next().is_some_and(|c| c.is_ascii_lowercase())) {
        if word == "max" {
            continue;
        }
        let window = st[word]["window"].as_str().unwrap_or_else(|| panic!("{word} is a status field"));
        assert_ne!(window, "24h", "{word} is a 24 h field in the distance");
    }
    assert_eq!(
        [host::SCRATCH_SHORT, host::DISK_SHORT, host::IO_PRESSURE, host::HOST_SHEDDING, host::SHED].map(|c| condition(&s, c)),
        ["True", "True", "True", "False", "False"]
    );
}

#[test]
fn a_term_that_cannot_be_read_publishes_nothing_and_the_section_ages_into_stale() {
    let w = world(2, 0, 0);
    assert!(w.run(&calm()).unwrap().published);
    w.r.clock.advance(16 * MIN);
    let mut blind = calm();
    blind.io = None;
    let rep = w.run(&blind).unwrap();
    assert_eq!((rep.published, rep.unreadable.clone()), (false, vec![host::IO]));
    let snap = w.r.ok(&["section", "snapshot"], None);
    let item = snap["sections"].as_array().unwrap().iter().find(|i| i["section"]["kind"] == host::NAME).unwrap().clone();
    assert_eq!(item["freshness"]["state"], json!("Stale"), "a guess is never carried");
    assert!(hs::distance(&blind, &hs::targets_of(None).unwrap_or(hs::Targets { scratch: 0.3, worktrees: 0.2, io: 40.0 })).is_none());
}

#[test]
fn a_shed_lowers_certify_par_by_one_from_to_and_raises_host_shedding_with_shed_from_recorded() {
    let w = world(3, 0, 0);
    w.run(&day_1031()).unwrap();
    let key = w.put("1", host::SHED_SLOT, 3, 2);
    let rep = w.run(&day_1031()).unwrap();
    assert_eq!(rep.acked, [(key.clone(), "applied".to_string())]);
    assert_eq!((w.par.par.get(), w.par.sets.borrow().as_slice()), (2, [2].as_slice()));
    assert_eq!(w.ack(&key)["result"], "applied");
    let s = w.section();
    assert_eq!(s["status"][host::SHED_FROM], json!({ "value": 3.0, "window": "level" }));
    assert_eq!(s["status"][host::CERTIFY_PAR]["value"], 2.0);
    assert_eq!(condition(&s, host::HOST_SHEDDING), "True");
    assert_eq!(condition(&s, host::SHED), "False", "the host is still over its spec");

    let second = w.put("2", host::SHED_SLOT, 2, 1);
    w.run(&day_1031()).unwrap();
    assert_eq!(w.ack(&second)["result"], "applied");
    assert_eq!(w.section()["status"][host::SHED_FROM]["value"], 3.0, "shed_from is where the first shed began");
    let third = w.put("3", host::SHED_SLOT, 1, 0);
    w.run(&day_1031()).unwrap();
    assert!(w.ack(&third)["result"]["refused"]["reason"].as_str().unwrap().contains("at least one"));
    assert_eq!(w.par.par.get(), 1);
}

#[test]
fn a_restore_undoes_only_a_shed_and_clears_shed_from_when_the_shed_is_undone() {
    let w = world(2, 0, 0);
    w.run(&calm()).unwrap();
    let none = w.put("1", host::RESTORE_SLOT, 2, 3);
    w.run(&calm()).unwrap();
    assert!(w.ack(&none)["result"]["refused"]["reason"].as_str().unwrap().contains("nothing to restore"));
    assert_eq!(w.par.par.get(), 2);

    w.put("2", host::SHED_SLOT, 2, 1);
    w.run(&calm()).unwrap();
    let s = w.section();
    assert_eq!((condition(&s, host::HOST_SHEDDING), condition(&s, host::SHED)), ("True".into(), "True".into()), "calm host: the shed is Shed");
    let back = w.put("3", host::RESTORE_SLOT, 1, 2);
    w.run(&calm()).unwrap();
    assert_eq!(w.ack(&back)["result"], "applied");
    let s = w.section();
    assert_eq!(w.par.par.get(), 2);
    assert!(s["status"].get(host::SHED_FROM).is_none(), "undone: nothing is in force");
    assert_eq!(condition(&s, host::HOST_SHEDDING), "False");
}

#[test]
fn a_restore_above_where_the_shed_began_is_refused() {
    let i = |from: u64, to: u64| {
        spira_desired_state::intent::parse_intent(&json!({
            "id": "1", "action": host::RESTORE_SLOT, "target": "knobs/Host/host/restore-gate-slot", "from": from, "to": to,
            "moves": "certify_par", "baseline": 1.0, "expected_effect": 1.0, "horizon": "15m",
            "addresses": ["Shed"], "taken_at": "2026-10-31T19:40:00Z", "by": { "pass": "p", "lock_token": "1.0" },
        }))
        .unwrap()
    };
    assert_eq!(hs::decide(&i(1, 2), 1, Some(2)), hs::Step::Set(2));
    assert!(matches!(hs::decide(&i(2, 3), 2, Some(2)), hs::Step::Refuse(m) if m.contains("above 2")));
    assert!(matches!(hs::decide(&i(2, 3), 2, None), hs::Step::Refuse(m) if m.contains("nothing to restore")));
    assert!(matches!(hs::decide(&i(1, 3), 1, Some(5)), hs::Step::Refuse(m) if m.contains("one step")));
    assert!(matches!(hs::decide(&i(4, 5), 2, Some(5)), hs::Step::Refuse(m) if m.starts_with("stale")));
    assert_eq!(hs::decide(&i(1, 2), 2, Some(2)), hs::Step::AlreadyApplied);
}

#[test]
fn a_reap_takes_a_tenth_of_the_stale_set_and_a_rerun_after_a_lost_ack_is_already_applied() {
    let w = world(2, 20, 20);
    w.run(&day_1031()).unwrap();
    assert_eq!(w.r.knob("knobs/Host/host/reap-stale-scratch"), json!(20), "the knob mirrors the stale count");
    let key = w.put("1", host::REAP_SCRATCH, 20, 18);
    let rep = w.run(&day_1031()).unwrap();
    assert_eq!(rep.acked, [(key.clone(), "applied".to_string())]);
    assert_eq!(w.stale.reaps.borrow().as_slice(), [(Stale::Scratch, 0.1, 2)], "exactly a tenth of 20, and only the scratch reap ran");
    assert_eq!(w.ack(&key)["ack"].clone(), Value::Null);
    assert_eq!(w.ack(&key)["result"], "applied");
    assert_eq!(w.stale.targets.get(), 20);
    assert_eq!(w.r.knob("knobs/Host/host/reap-stale-scratch"), json!(18));

    let again = w.put("2", host::REAP_SCRATCH, 20, 18);
    w.run(&day_1031()).unwrap();
    assert_eq!(w.ack(&again)["result"], "already-applied");
    assert_eq!(w.stale.reaps.borrow().len(), 1, "nothing was reaped twice");
    let grows = w.put("3", host::REAP_TARGETS, 18, 19);
    w.run(&day_1031()).unwrap();
    assert!(w.ack(&grows)["result"]["refused"]["reason"].as_str().unwrap().contains("does not shrink"));
}

#[test]
fn an_unchanged_section_is_published_again_only_at_its_keepalive_and_a_silent_watchtower_reads_stale() {
    let w = world(2, 0, 0);
    assert!(w.run(&calm()).unwrap().published);
    w.r.clock.advance(MIN);
    assert!(!w.run(&calm()).unwrap().published, "the same status inside the keepalive");
    w.r.clock.advance(5 * MIN);
    assert!(w.run(&calm()).unwrap().published, "the keepalive");
    let freshness = |r: &Rig<MemBackend>| {
        let snap = r.ok(&["section", "snapshot"], None);
        let item = snap["sections"].as_array().unwrap().iter().find(|i| i["section"]["kind"] == host::NAME).unwrap().clone();
        item["freshness"]["state"].clone()
    };
    assert_eq!(freshness(&w.r), json!("Fresh"));
    w.r.clock.advance(16 * MIN);
    assert_eq!(freshness(&w.r), json!("Stale"), "when the watchtower stops publishing");
}

#[test]
fn a_config_that_cannot_state_certify_par_stops_the_pass_before_it_acks_anything() {
    struct Unreadable;
    impl Config for Unreadable {
        fn get(&self, key: &str) -> Result<u64, String> {
            Err(format!("{key} is unreadable"))
        }

        fn set(&self, _: &str, _: u64) -> Result<(), String> {
            panic!("nothing is set against a value that could not be read")
        }
    }
    let w = world(2, 0, 0);
    let d = Dispatch(&w.r);
    let e = hs::run(&Ports { lc: &d, config: &Unreadable, reaper: &w.stale, record: &|_| {} }, secs(&w.r), &calm()).unwrap_err();
    assert!(e.contains("unreadable"), "{e}");
}
