//! The cockpit tiers' store reads are answered by the watched views until a change ends them: a
//! scheduler's tiers against a counting store socket, with a projection installed on it. The
//! watch is polled by hand, so a change is applied before the next observe. One test in one
//! file: the projection installs once per process, and the rig edits the environment.

#[macro_use]
extern crate spira_test_id;

use spira_runtime::tasks::g::cockpit_tier_5s as t5;
use spira_runtime::tasks::g::cockpit_tier_600s as t600;
use spira_runtime::tasks::g::cockpit_tier_60s as t60;
use std::collections::{BTreeMap, VecDeque};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use runtime::event::EffectSpec;
use serde_json::{json, Value};
use spira_runtime::tasks::g::cockpit::{self, Carry, Snapshot, WRITE_KEY};
use spira_runtime::world::World;
use spira_runtime::Effects;
use spira_store::lc_call::LcClient;
use store_view::Projection;

#[derive(Default)]
struct Store {
    /// Every request but the watch and the views' own load.
    asked: Vec<String>,
    asks: Vec<Value>,
    watches: VecDeque<String>,
}

impl Store {
    fn answer(&mut self, argv: &[String]) -> String {
        let word = |n: usize| argv.get(n).map(String::as_str).unwrap_or_default();
        match word(0) {
            "watch" => return self.watches.pop_front().unwrap_or_else(|| json!({ "progress_rev": 5 }).to_string()),
            "rc" => return json!({ "ok": { "items": [], "list_rev": 5 } }).to_string(),
            _ => self.asked.push(argv.join(" ")),
        }
        match (word(0), word(1)) {
            ("list", _) if !argv.iter().any(|a| a == "--batches") => rows(argv),
            ("content", "list" | "show") => content(),
            ("list-asks", _) => Value::Array(self.asks.clone()).to_string(),
            ("claimable", _) => json!({ "rev": 5, "cannot_tell": 0, "beads": [] }).to_string(),
            ("section", "snapshot") => json!({ "sections": [], "intents": [], "knobs": {}, "pass": null, "list_rev": 5 }).to_string(),
            ("plan", "repo-labels") => json!({ "watermark": 1, "unmapped": 0, "absent": 0 }).to_string(),
            ("plan", _) => json!({ "watermark": 1, "items": [], "edges": [] }).to_string(),
            ("condition", _) => json!({ "conditions": [] }).to_string(),
            _ => "[]".to_string(),
        }
    }
}

struct Rig {
    _dir: testkit::TempDir,
    _env: testkit::EnvGuard,
    store: Arc<Mutex<Store>>,
    views: Arc<Projection>,
    pub run: PathBuf,
    fx: Effects,
}

fn rig(name: &str) -> Rig {
    let dir = testkit::TempDir::new(name);
    let (run, mail) = (dir.join("run"), dir.join("mail"));
    for d in [&run, &mail.join("concierge/new"), &dir.join("spira")] {
        std::fs::create_dir_all(d).unwrap();
    }
    // The configured client names the release's `spira-lc` even on the socket route; none is run.
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), dir.join("bin/spira-lc")).unwrap();
    let socket = dir.join("lc.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let store = Arc::new(Mutex::new(Store::default()));
    let served = store.clone();
    std::thread::spawn(move || runtime::socket::answer(&listener, &|argv| Ok(served.lock().unwrap().answer(argv))));

    let map = spira_config::process::fixture_repo_map(dir.path(), &["spira"]);
    let text = |p: &std::path::Path| p.display().to_string();
    let toml = spira_config::process::fixture_toml(
        dir.path(),
        &[("SPIRA_RUN", &text(&run)), ("SPIRA_REPO_MAP", &text(&map)), ("SPIRA_HOME_REPO", "spira"), ("SPIRA_WORKSPACES", &text(dir.path())), ("SPIRA_LC_SOCKET", &text(&socket)), ("SPIRA_MAIL", &text(&mail))],
    );
    let home = spira_config::process::fixture_home();
    let env = testkit::env(&[("SPIRA_TOML", Some(&text(&toml))), ("SPIRA_HOME", Some(&text(&home))), ("SPIRA_RUN", Some(&text(&run))), ("SPIRA_RELEASE", Some(&text(dir.path()))), ("SPIRA_LC_BIN", None), ("SPIRA_REPO_MAP", None), ("SPIRA_HOME_REPO", None), ("SPIRA_BDJSON_FIXTURE", None)]);

    let frozen = Instant::now();
    let views = Projection::new(LcClient::socket(&socket), Duration::from_secs(600), Arc::new(move || frozen)).with_prefixes(&spira_runtime::tasks::watched_prefixes());
    let views = Arc::new(views);
    assert!(Arc::ptr_eq(&store_view::install(views.clone()), &views), "the first install stands");
    views.load().unwrap();

    let units = Arc::new(runtime::units::ScriptedUnits::new(Arc::new(runtime::ports::VirtualClock::new(0))));
    let state = dir.join("vm");
    std::fs::create_dir_all(&state).unwrap();
    let pool = round_vm::pool::Pool { state_dir: state, retry_interval: Duration::ZERO, max_retries: 1, wait_poll: Duration::from_millis(10), acquire_deadline: Duration::ZERO };
    let world = World { run_dir: run.clone(), lc_socket: socket, cockpit_view_dir: dir.join("runtime/spira-cockpit"), ..World::default() };
    let fx = Effects { world, units: Some(units), vm_pool: Some(Arc::new(pool)), ..Effects::default() };
    Rig { _dir: dir, _env: env, store, views, run, fx }
}

impl Rig {
    fn client(&self) -> LcClient {
        LcClient::socket(&self.fx.world.lc_socket)
    }

    /// One observe of a tier, every probe given a minute.
    fn observe(&self, probes: &[(&str, u64)]) -> Snapshot {
        let probes: Vec<(&str, u64)> = probes.iter().map(|(n, _)| (*n, 60_000)).collect();
        let cfg: BTreeMap<String, String> = [(WRITE_KEY.to_string(), "1".to_string())].into();
        let spec = EffectSpec::probe("observe", cockpit::probe_input(&probes, &cfg), 60_000);
        serde_json::from_str(&cockpit::run_observe(&self.fx, "", &spec).unwrap()).unwrap()
    }

    /// The requests since the last call.
    fn asked(&self) -> Vec<String> {
        std::mem::take(&mut self.store.lock().unwrap().asked)
    }

    fn watch(&self) -> store_view::Watching<'_> {
        self.views.hold_watching()
    }

    fn open_ask(&self, id: &str) {
        self.store.lock().unwrap().asks.push(json!({ "ask_id": id, "state": "OPEN", "work_bead": null, "opened_at": "100", "version": "1", "title": "decide" }));
    }

    /// Delivers a change of `key` and returns once the views have applied it.
    fn changed(&self, key: &str) {
        let event = json!({ "key": key, "rev": { "main": 9, "sub": 0 }, "value": null });
        self.store.lock().unwrap().watches.push_back(json!({ "events": [event], "through": 9, "rows": [] }).to_string());
        self.views.poll().unwrap();
    }

    /// Publishes `tier`'s snapshot and answers `run/cockpit.env`.
    fn publish(&self, effect: &str, input: &str) -> String {
        cockpit::run_publish(&self.fx, "", &EffectSpec::keyed(effect, "k", input, 1_000)).unwrap();
        std::fs::read_to_string(self.run.join("cockpit.env")).unwrap()
    }
}

/// `env`'s lines but the keys that read the clock.
fn but_clock(env: &str) -> Vec<&str> {
    env.lines().filter(|l| l.split_once('=').is_none_or(|(k, _)| k != "SP_AT" && !k.starts_with("_PROBE_AT_") && !k.ends_with("_AGE") && !k.ends_with("_AGE_S") && !k.ends_with("_AGE_M"))).collect()
}

/// Four beads, one of them landed, and a content list that also names one with no row at all.
fn rows(argv: &[String]) -> String {
    let named = |flag: &str| argv.iter().position(|a| a == flag).map(|i| argv[i + 1].as_str());
    let row = |id: &str, state: &str, holds: &str| {
        let view = matches!(state, "READY" | "REWORK").then(|| json!({ "verdict": "claimable", "blocker": null, "runes": ["builder"] }));
        json!({ "bead_id": id, "state": state, "holds": holds, "express": "0", "repo": "spira", "title": id, "priority": 1, "updated_at": 100, "since": 100, "claim_view": view })
    };
    let all = [row("sp-a", "READY", "[]"), row("sp-b", "WORKING", "[]"), row("sp-c", "CERTIFIED", "[\"poison\"]"), row("sp-d", "LANDED", "[]")];
    let kept: Vec<&Value> = all
        .iter()
        .filter(|r| named("--ids").is_none_or(|ids| ids.split(',').any(|i| r["bead_id"] == i)))
        .filter(|r| named("--state").is_none_or(|states| states.split(',').any(|s| r["state"] == s)))
        .filter(|r| named("--hold").is_none_or(|h| r["holds"].as_str().is_some_and(|held| held.contains(h))))
        .filter(|r| !argv.iter().any(|a| a == "--live") || r["state"] != "LANDED")
        .filter(|r| !argv.iter().any(|a| a == "--express") || r["express"] == "1")
        .collect();
    serde_json::to_string(&kept).unwrap()
}

fn content() -> String {
    let item = |id: &str| json!({ "id": id, "title": format!("title of {id}"), "priority": 1, "issue_type": "task", "status": "open", "labels": ["plan", "spira", "repo:spira"], "dependencies": [], "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z" });
    Value::Array(["sp-a", "sp-b", "sp-c", "sp-d", "sp-epic"].map(item).to_vec()).to_string()
}

/// `decide`'s first publish of `snap`, as `run/cockpit.env` then reads.
fn published(r: &Rig, snap: &Snapshot, t: Tier) -> String {
    let (_, acts) = (t.decide)(&Carry::default(), snap).unwrap();
    let publish = acts.iter().find(|a| a.name == t.publish).expect("a first snapshot is published");
    r.publish(t.publish, &publish.input)
}

type Decide = fn(&Carry, &Snapshot) -> Result<(Carry, Vec<EffectSpec>), String>;

#[derive(Clone, Copy)]
struct Tier {
    probes: &'static [(&'static str, u64)],
    decide: Decide,
    publish: &'static str,
}

const MINUTE: Tier = Tier { probes: t60::PROBES, decide: t60::decide, publish: t60::PUBLISH };
const TEN_MINUTES: Tier = Tier { probes: t600::PROBES, decide: t600::decide, publish: t600::PUBLISH };
const FIVE: Tier = Tier { probes: t5::PROBES, decide: t5::decide, publish: t5::PUBLISH };

/// An unwatched observe reads each of `verbs`; two watched ones publish the same, the second
/// asking the store nothing.
fn quiet_after_one_read(r: &Rig, t: Tier, verbs: &[&str]) {
    let unheld = published(r, &r.observe(t.probes), t);
    let reads = r.asked();
    for verb in verbs {
        assert!(reads.iter().any(|a| a == verb || a.starts_with(&format!("{verb} "))), "an unwatched observe reads {verb}: {reads:?}");
    }
    let _watching = r.watch();
    let first = published(r, &r.observe(t.probes), t);
    r.asked();
    let second = published(r, &r.observe(t.probes), t);
    assert_eq!(r.asked(), Vec::<String>::new(), "nothing changed since the reads were answered");
    assert_eq!(but_clock(&first), but_clock(&unheld), "a held read publishes what the store's read did");
    assert_eq!(but_clock(&second), but_clock(&unheld));
}

#[test_id(t0k5v28)]
#[test]
fn a_second_observe_of_an_unchanged_store_sends_it_nothing_and_an_ask_change_rereads_the_asks() {
    let r = rig("cockpit-tiers-idle");

    quiet_after_one_read(&r, FIVE, &["list-asks", "mending list --live", "ops-view ops_recent"]);
    quiet_after_one_read(&r, MINUTE, &["list", "claimable", "list-asks", "ops-graph", "ops-view ops_dwell", "ops-view ops_edges", "plan repo-labels", "section snapshot", "content list"]);

    // The slow tier also reads memories and gates, which no store key ends, so only its lifecycle
    // and bead content reads are held to silence.
    let held = |a: &String| ["list ", "content list", "content show"].iter().any(|v| a.starts_with(v));
    r.observe(TEN_MINUTES.probes);
    let reads = r.asked();
    for verb in ["list --live", "content list"] {
        assert!(reads.iter().any(|a| a.starts_with(verb)), "an unwatched slow observe reads {verb}: {reads:?}");
    }
    {
        let _watching = r.watch();
        r.observe(TEN_MINUTES.probes);
        r.asked();
        r.observe(TEN_MINUTES.probes);
        assert_eq!(r.asked().into_iter().filter(held).collect::<Vec<_>>(), Vec::<String>::new(), "the slow tier re-reads no bead or row");
        r.changed("planning/watermark");
        r.observe(TEN_MINUTES.probes);
        assert!(r.asked().iter().any(|a| a.starts_with("content list")), "a planning write ends the content it read");
    }

    let _watching = r.watch();
    let actor = spira_claim::store::Store { lc: r.client(), lc_bin: String::new(), timeout: Duration::from_secs(5), views: store_view::shared() };
    actor.claim_view().unwrap();
    actor.claim_view().unwrap();
    assert_eq!(r.asked(), ["claimable", "claimable"], "a claim reader that acts is never answered from the held view");
    r.observe(MINUTE.probes);
    r.observe(FIVE.probes);
    r.asked();
    let before = published(&r, &r.observe(MINUTE.probes), MINUTE);
    assert!(before.lines().any(|l| l == "SP_WAITING='0'"), "{before}");

    r.open_ask("sp-ask");
    r.changed("asks/sp-ask");
    r.observe(FIVE.probes);
    assert_eq!(r.asked(), ["list-asks"], "the 5 s tier re-reads the changed asks once, and nothing else");
    r.open_ask("sp-other");
    r.changed("asks/sp-other");
    let after = published(&r, &r.observe(MINUTE.probes), MINUTE);
    assert_eq!(r.asked(), ["list-asks"], "and so does the minute tier");
    assert!(after.lines().any(|l| l == "SP_WAITING='2'"), "{after}");
}
