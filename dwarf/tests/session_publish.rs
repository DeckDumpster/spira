// covers: dwarf/src/session_feed.rs dwarf/src/run.rs
//! A whole run, claim to teardown, publishing its session to a real broker. The model session is
//! a fake launcher; git is real.
#[macro_use]
extern crate spira_test_id;

use std::collections::BTreeMap;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use dwarf::conf::{Conf, Rune};
use dwarf::ledger::Ledger;
use dwarf::ports::{Bd, Env, Exec, RealGit, Seam};
use dwarf::run::{Deps, Mode, Run, State};
use dwarf::seam::Snapshot;
use dwarf::session::{Launcher, SessionSpec, Stop};
use dwarf::util::{Out, Sink};
use rumqttc::{Client, Connection, Event, MqttOptions, Packet, QoS, Transport};
use serde_json::{json, Value};
use testkit::broker::Broker;

/// Bounds each wait on the broker or the heartbeat so a broken one fails instead of hanging.
const FAILSAFE: Duration = Duration::from_secs(30);
const NOW: i64 = 1_800_000_000;
const SESSION_RC: i32 = 7;
const STATE: &str = "session/sp-x/state";

struct Subscriber {
    _client: Client,
    connection: Connection,
}

impl Subscriber {
    fn of(socket: &Path, id: &str) -> Subscriber {
        let mut options = MqttOptions::new(id, socket.to_str().unwrap(), 0);
        options.set_transport(Transport::Unix);
        options.set_keep_alive(Duration::ZERO);
        let (client, mut connection) = Client::new(options, 32);
        client.subscribe(STATE, QoS::AtLeastOnce).unwrap();
        loop {
            match connection.recv_timeout(FAILSAFE) {
                Ok(Ok(Event::Incoming(Packet::SubAck(_)))) => break,
                Ok(Ok(_)) => {}
                other => panic!("never subscribed: {other:?}"),
            }
        }
        Subscriber { _client: client, connection }
    }

    fn next(&mut self) -> Value {
        loop {
            match self.connection.recv_timeout(FAILSAFE) {
                Ok(Ok(Event::Incoming(Packet::Publish(p)))) => return serde_json::from_slice(&p.payload).unwrap(),
                Ok(Ok(_)) => {}
                other => panic!("no message arrived: {other:?}"),
            }
        }
    }
}

/// The lifecycle store, the claim, and the seam of one directed claim of sp-x.
#[derive(Default)]
struct Store {
    holder: Mutex<String>,
    calls: Mutex<Vec<String>>,
    renewed: Condvar,
}

impl Store {
    fn await_renewal(&self) {
        let calls = self.calls.lock().unwrap();
        let (_calls, waited) = self.renewed.wait_timeout_while(calls, FAILSAFE, |c| !c.iter().any(|v| v == "renew")).unwrap();
        assert!(!waited.timed_out(), "the heartbeat never renewed");
    }

    /// Each lifecycle call's verb, a run of the same verb counted once.
    fn verbs(&self) -> Vec<String> {
        let mut v = self.calls.lock().unwrap().clone();
        v.dedup();
        v
    }
}

impl Exec for Store {
    fn exec(&self, prog: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
        let verb = args.first().map(String::as_str).unwrap_or("");
        if prog == "spira-lc" && verb != "show" {
            self.calls.lock().unwrap().push(verb.to_string());
            self.renewed.notify_all();
        }
        match (prog, verb) {
            ("spira-claim", "stack") => Out::ok(r#"{"claimable":true,"stack":{},"stack_depth":0,"stack_max_depth":4}"#),
            ("spira-claim", "rejections") => Out::ok(r#"{"rejections":0,"stronger_rejections":0,"history":[]}"#),
            ("spira-lc", "list") => Out::ok("[]"),
            ("spira-lc", "show") => Out::ok(json!({"bead": {"bead_id": "sp-x", "state": "WORKING", "holder": *self.holder.lock().unwrap(), "holds": [], "builder_claims": "4"}}).to_string()),
            _ => Out::ok(""),
        }
    }
}

impl Seam for Store {
    fn call(&self, f: &str, a: &[String]) -> Out {
        if f == "lc_claim_bead" {
            *self.holder.lock().unwrap() = a[1].clone();
        }
        Out::ok("")
    }
}

struct Beads;
impl Bd for Beads {
    fn bd(&self, args: &[String]) -> Out {
        match args.first().map(String::as_str) {
            Some("show") => Out::ok(r#"[{"id":"sp-x","title":"t","description":"d","labels":[]}]"#),
            _ => Out::ok("[]"),
        }
    }
}

#[derive(Default)]
struct Lines(Mutex<Vec<String>>);
impl Sink for Lines {
    fn out(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_string());
    }
    fn err(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_string());
    }
}

/// A panic in the session is held until the run ends: unwinding out of `Run::main` would wait
/// forever on a heartbeat nothing told to stop.
struct Session<'a> {
    during: Box<dyn Fn() + Send + Sync + 'a>,
    panicked: Mutex<Option<Box<dyn std::any::Any + Send>>>,
}
impl Launcher for Session<'_> {
    fn run(&self, _: &SessionSpec, _: &Stop) -> i32 {
        if let Err(why) = catch_unwind(AssertUnwindSafe(|| (self.during)())) {
            *self.panicked.lock().unwrap() = Some(why);
        }
        SESSION_RC
    }
}

struct Outcome {
    exit: i32,
    session_rc: i32,
    verbs: Vec<String>,
    lease_written: bool,
    log: String,
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// `Run::main` over a fresh run directory and repository, publishing to `socket`;
/// `during` is the model session.
fn run(socket: &Path, store: &Store, during: impl Fn() + Send + Sync) -> Outcome {
    let dir = testkit::TempDir::new("dwarf-pub");
    let run_dir = dir.join("run");
    let repo = dir.join("repo");
    let home = dir.join("home");
    for d in [&run_dir, &repo, &home.join("runes")] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(home.join("runes/builder.md"), "You are a builder.").unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let map = dir.join("repos");
    std::fs::write(&map, format!("shared {} pr main\n", repo.display())).unwrap();
    let bin = dir.join("model-bin");
    std::fs::create_dir_all(&bin).unwrap();
    testkit::write_exe(bin.join("work"), "#!/bin/sh\n");

    let doc = spira_config::validate(&spira_config::process::complete_fixture_text()).unwrap();
    let mut vars: BTreeMap<String, String> = spira_config::spira_string_map(&doc)
        .into_iter()
        .map(|(k, v)| if k.starts_with("COCKPIT_") { (k, v) } else { (format!("SPIRA_{k}"), v) })
        .collect();
    let toml = spira_config::resolve::locate_home_for_process().unwrap().join("testlib/complete.toml");
    for (k, v) in [
        ("SPIRA_TOML", toml.display().to_string()),
        ("SPIRA_RUN", run_dir.display().to_string()),
        ("SPIRA_REPO", repo.display().to_string()),
        ("SPIRA_HOME_REPO", "shared".to_string()),
        ("SPIRA_REPO_MAP", map.display().to_string()),
        ("SPIRA_MAIL", dir.join("mail").display().to_string()),
        ("SPIRA_BROKER_SOCKET", socket.display().to_string()),
        ("SPIRA_METRICS_ENDPOINT", String::new()),
        ("SPIRA_SCOPE_LABEL", String::new()),
        ("SPIRA_ADMIT_JITTER_SECS", "0".to_string()),
    ] {
        vars.insert(k.to_string(), v);
    }
    let process: BTreeMap<String, String> = std::env::vars().filter(|(k, _)| k == "PATH" || k == "HOME" || k == "TMPDIR").collect();
    let env = Env::new(process.clone(), process);
    env.set(spira_config::release_env::MODEL_BIN_OVERRIDE_ENV, &bin.display().to_string());
    let conf = Conf::new(&Snapshot { vars, ..Default::default() }, &home);
    let mut rune = Rune::from_vars("builder", &BTreeMap::from([("RUNE_LABELS".to_string(), "plan".to_string())]));
    rune.static_prompt = true;
    rune.heartbeat_seconds = 1;
    let lease = run_dir.join("dwarf/sp-x.lease");
    let lease_written = AtomicBool::new(false);
    let session = Session {
        during: Box::new(|| {
            store.await_renewal();
            lease_written.store(lease.is_file(), std::sync::atomic::Ordering::SeqCst);
            during();
        }),
        panicked: Mutex::new(None),
    };
    let sink = Lines::default();
    let git_port = RealGit { env: &env };
    let mut r = Run {
        d: Deps { bd: &Beads, seam: store, git: &git_port, exec: store, launcher: &session, sink: &sink, env: &env, clock: &|| NOW, sleep: &|_| {} },
        ledger: Ledger { path: run_dir.join("dwarf-ledger.log"), dry: false },
        conf,
        rune,
        snap: Snapshot::default(),
        mode: Mode::Directed(dwarf::direct::Direct { bead: "sp-x".into(), by: "test".into(), reason: "test".into(), replaced: None }),
        pid: std::process::id(),
        own_unit: String::new(),
        t0: NOW,
        claim_bin: "spira-claim".into(),
        stop: Arc::new(Stop::default()),
        hb_shutdown: Arc::new(AtomicBool::new(false)),
        hb_done: Arc::new(AtomicBool::new(false)),
        rune_file: PathBuf::new(),
        s: State::default(),
    };
    let cwd = std::env::current_dir().unwrap();
    let exit = r.main();
    std::env::set_current_dir(cwd).unwrap();
    if let Some(why) = session.panicked.lock().unwrap().take() {
        resume_unwind(why);
    }
    let session_rc = r.s.session_rc;
    let log = sink.0.lock().unwrap().join("\n");
    Outcome { exit, session_rc, verbs: store.verbs(), lease_written: lease_written.load(std::sync::atomic::Ordering::SeqCst), log }
}

/// A run moves this process into its worktree, so one at a time.
static ONE_RUN: Mutex<()> = Mutex::new(());

#[test_id(trt6vvp)]
#[test]
fn a_run_publishes_its_session_started_then_ended_and_runs_the_same_with_the_broker_stopped() {
    let _one = ONE_RUN.lock().unwrap_or_else(|e| e.into_inner());
    let mut broker = Broker::up();
    let watching = Mutex::new(Subscriber::of(broker.socket(), "watching"));
    let seen = Mutex::new(Vec::new());
    let up = run(broker.socket(), &Store::default(), || seen.lock().unwrap().push(watching.lock().unwrap().next()));
    let mut watching = watching.into_inner().unwrap();
    let started = seen.lock().unwrap().pop().unwrap_or_else(|| panic!("no session ran:\n{}", up.log));
    assert_eq!(started, json!({"state": "started", "bead": "sp-x", "rune": "builder", "started_at": NOW, "sessions": 4}), "{}", up.log);
    assert_eq!(watching.next(), json!({"state": "ended", "rc": SESSION_RC, "wall_s": 0, "at": NOW}));
    assert_eq!(Subscriber::of(broker.socket(), "late").next()["state"], "ended", "the last state is retained");
    assert_eq!(up.session_rc, SESSION_RC, "positive control: the session ran\n{}", up.log);

    broker.stop();
    let down = run(broker.socket(), &Store::default(), || {});
    assert!(down.lease_written && up.lease_written, "{}", down.log);
    assert_eq!((down.exit, down.session_rc, &down.verbs), (up.exit, up.session_rc, &up.verbs), "{}", down.log);
    assert!(down.verbs.iter().any(|v| v == "renew"), "{:?}", down.verbs);
}

#[test_id(twonu66)]
#[test]
fn a_broker_restarted_during_a_session_is_told_the_session_state_again() {
    let _one = ONE_RUN.lock().unwrap_or_else(|e| e.into_inner());
    let broker = Mutex::new(Broker::up());
    let socket = broker.lock().unwrap().socket().to_path_buf();
    let told = Mutex::new(None);
    let outcome = run(&socket, &Store::default(), || {
        let before = Subscriber::of(&socket, "before").next();
        broker.lock().unwrap().restart();
        let after = Subscriber::of(&socket, "after").next();
        *told.lock().unwrap() = Some((before, after));
    });
    let (before, after) = told.lock().unwrap().take().unwrap_or_else(|| panic!("no session ran:\n{}", outcome.log));
    assert_eq!(before["state"], "started");
    assert_eq!(after, before);
}
