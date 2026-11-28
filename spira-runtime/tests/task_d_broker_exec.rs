//! `broker.exec` against the real pieces: the `work` client, `spira-lc serve` and `spira-lc
//! work-exec` as processes, the scheduler's effects run in this one, and a stub `groomer` in the
//! release's `bin/` for the program a tool verb starts. The service records the verb and answers
//! at once; the program runs in `work-exec`, never in the service.

#[macro_use]
extern crate spira_test_id;

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use runtime::host::EffectRunner;
use spira_otel::fixture::{spans_in, Collector, Post};
use spira_runtime::tasks::d::broker_exec::{self, Snapshot};
use spira_runtime::Effects;
use state_store_sqlite::{Options, SqliteBackend};

struct World {
    collector: Collector,
    _env: testkit::EnvGuard,
    dir: testkit::TempDir,
    toml: PathBuf,
    socket: PathBuf,
    server: Child,
}

impl World {
    fn release(&self) -> PathBuf {
        self.dir.join("release")
    }

    fn stub(&self, name: &str, body: &str) {
        testkit::write_exe(self.release().join("bin").join(name), &format!("#!/bin/sh\n{body}\n"));
    }

    fn command(&self, bin: &str) -> Command {
        let mut c = Command::new(testkit::bin_exe(bin));
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("SPIRA_TOML", &self.toml)
            .env("SPIRA_HOME", spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree"))
            .env("SPIRA_RELEASE", self.release())
            .env("HOME", self.dir.join("home"));
        c
    }

    fn effects(&self) -> Effects {
        let world = spira_runtime::world::World { run_dir: self.dir.join("run"), lc_socket: self.socket.clone(), ..Default::default() };
        Effects { world, ..Effects::default() }
    }

    /// One scheduler pass: observe, decide, run each effect it decided.
    fn tick(&self, e: &Effects) -> Vec<String> {
        let spec = runtime::EffectSpec::probe(broker_exec::OBSERVE, "", 10_000);
        let snap: Snapshot = serde_json::from_str(&e.run(&spec.key, &spec).expect("the probe answers")).unwrap();
        broker_exec::decide(&snap).iter().map(|s| {
            self.journal_open(&s.key);
            e.run(&s.key, s).unwrap_or_else(|err| format!("failed: {err}"))
        }).collect()
    }

    /// What the engine commits before a runner starts: the effect's open row, under its request id.
    fn journal_open(&self, id: &str) {
        let row = serde_json::json!({"task": "broker.exec", "ekey": id, "request_id": id, "stream": "broker.exec", "spec": {}, "requested_at": 1, "deadline_at": 2});
        spira_store::lc_call::LcClient::socket(&self.socket).answer(&["journal", "turn", &serde_json::json!({"effects": [row]}).to_string()]).expect("the turn commits");
    }

    /// Runs the `work` client with `args` while the scheduler ticks, and returns its exit code and output.
    fn work(&self, persona: &str, args: &[&str]) -> (i32, String, Vec<String>) {
        let e = self.effects();
        let child = self.command("work").env("SPIRA_RUNE", persona).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("work runs");
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let out = child.wait_with_output().unwrap();
            let _ = tx.send(());
            out
        });
        let mut ran = Vec::new();
        while rx.recv_timeout(Duration::from_millis(50)).is_err() { // clock-safe: the scheduler's tick cadence; the client's exit is the signal waited on
            ran.extend(self.tick(&e));
        }
        let out = waiter.join().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text, ran)
    }

    fn spans(&self, name: &str) -> Vec<serde_json::Value> {
        assert!(self.collector.wait_until(|p| spans_in(p).iter().any(|s| s.name == name), Duration::from_secs(30)), "no `{name}` span reached the collector");
        self.collector.spans_json().into_iter().filter(|s| s["name"] == name).collect()
    }
}

/// Whether `post` carries the service's span for a `work` call it answered: a cut call also sends one.
fn answered_work_span(post: &Post) -> bool {
    spans_in(std::slice::from_ref(post)).iter().any(|s| s.name == "work" && !s.attributes.iter().any(|a| a.key == "spira.serve.cut" && format!("{:?}", a.value).contains("BoolValue(true)")))
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
    }
}

fn world() -> World {
    let dir = testkit::TempDir::new("broker-exec");
    let run = dir.join("run");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::create_dir_all(dir.join("release/bin")).unwrap();
    std::fs::create_dir_all(dir.join("home")).unwrap();
    let store = dir.join("control.db");
    drop(SqliteBackend::open(&store, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap());
    let socket = dir.join("lc.sock");
    let marker = dir.join("span-received");
    let collector = Collector::start_then(move |post| {
        if answered_work_span(post) {
            std::fs::write(&marker, "x").unwrap();
        }
    });
    let (d, r, s, sock) = (dir.display().to_string(), run.display().to_string(), store.display().to_string(), socket.display().to_string());
    let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_WORKSPACES", &d), ("SPIRA_RUN", &r), ("SPIRA_LC_STORE_PATH", &s), ("SPIRA_LC_SOCKET", &sock), ("SPIRA_VICTORIALOGS_URL", "http://127.0.0.1:9"), ("SPIRA_OTEL_COLLECTOR_ENDPOINT", collector.addr())]);
    let mut server = Command::new(testkit::bin_exe("spira-lc"))
        .arg("serve")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("SPIRA_TOML", &toml)
        .env("SPIRA_HOME", spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree"))
        .env("SPIRA_RELEASE", dir.join("release"))
        .env("HOME", dir.join("home"))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spira-lc serve runs");
    let mut stderr = BufReader::new(server.stderr.take().unwrap());
    let (mut line, mut said) = (String::new(), String::new());
    while !line.contains(" ready after ") {
        line.clear();
        assert!(stderr.read_line(&mut line).unwrap() > 0, "the server stopped before it said ready: {said}");
        said.push_str(&line);
    }
    std::thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::sink()));
    let env = testkit::env(&[
        ("SPIRA_TOML", Some(toml.to_str().unwrap())),
        ("SPIRA_RELEASE", Some(dir.join("release").to_str().unwrap())),
        ("SPIRA_HOME", Some(spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree").to_str().unwrap())),
        ("HOME", Some(dir.join("home").to_str().unwrap())),
    ]);
    let w = World { collector, _env: env, dir, toml, socket, server };
    std::os::unix::fs::symlink(testkit::bin_exe("spira-lc"), w.release().join("bin/spira-lc")).unwrap();
    w
}

fn attr(span: &serde_json::Value, key: &str) -> Option<i64> {
    let a = span["attributes"].as_array()?.iter().find(|a| a["key"] == key)?;
    a["value"]["intValue"].as_i64().or_else(|| a["value"]["intValue"].as_str()?.parse().ok())
}

fn marker(w: &World, name: &str) -> String {
    std::fs::read_to_string(w.dir.join(name)).unwrap_or_default()
}

/// The program a stub writes its parent's parent's command line to: that process is whoever ran
/// the tool, which must be `work-exec` and not the service.
fn who_ran_me(w: &World, out: &str) -> String {
    format!(
        "ppid=$(awk '{{print $4}}' /proc/$PPID/stat)\ntr '\\0' ' ' < /proc/$ppid/cmdline > {}\necho x >> {}.count\necho {out}",
        w.dir.join("ran-by").display(),
        w.dir.join("ran").display()
    )
}

#[test_id(t0yqx29)]
#[test]
fn a_tool_verb_is_recorded_by_the_service_and_run_by_the_scheduler_in_work_exec() {
    let w = world();
    // clock-safe: the signal waited on is the collector's marker that the service's `work` span arrived; the shell sleep only caps that wait, under the client's own.
    let wait_for_span = format!(
        "n=0\nuntil [ -e {received} ]; do\n  n=$((n+1))\n  [ $n -gt 600 ] && break\n  sleep 0.1\ndone\nif [ $n -gt 600 ]; then : > {missing}; else : > {first}; fi",
        received = w.dir.join("span-received").display(),
        missing = w.dir.join("span-missing").display(),
        first = w.dir.join("span-first").display()
    );
    w.stub("groomer", &format!("{wait_for_span}\n{}", who_ran_me(&w, "swept")));
    let (code, out, ran) = w.work("groomer", &["groom", "sweep"]);
    assert!(w.dir.join("span-first").exists(), "the service's `work` span was not written while the program ran: the service waited for the program (client: {code} {out}; ran: {ran:?})");
    assert_eq!((code, out.trim()), (0, "swept"), "the client prints the program's answer: {out}");
    assert!(ran.iter().any(|r| r.contains("ran, exit 0")), "the scheduler ran the request: {ran:?}");
    assert!(marker(&w, "ran-by").contains("work-exec"), "the program ran under work-exec, not the service: {}", marker(&w, "ran-by"));
    let spans = w.spans("work");
    assert_eq!(spans.len(), 1, "the service served the one verb: {spans:?}");
    assert_eq!(attr(&spans[0], "spira.children"), None, "the service started no program for it: {:?}", spans[0]["attributes"]);
}

#[test_id(tbheoky)]
#[test]
fn a_killed_runner_leaves_the_request_finished_124_and_the_client_prints_why() {
    let w = world();
    w.stub("groomer", "ppid=$(awk '{print $4}' /proc/$PPID/stat)\nkill -9 $ppid\nsleep 1");
    let (code, out, _) = w.work("groomer", &["groom", "sweep"]);
    assert_eq!(code, 124, "{out}");
    assert!(out.contains("before recording an answer") && out.contains("stage: running"), "the reason names how the runner ended: {out}");
}

#[test_id(t5rmm18)]
#[test]
fn a_request_the_ledger_has_run_is_not_run_again_after_the_scheduler_restarts() {
    let w = world();
    w.stub("groomer", &who_ran_me(&w, "swept"));
    let (code, _, ran) = w.work("groomer", &["groom", "sweep"]);
    assert_eq!(code, 0);
    assert_eq!(marker(&w, "ran.count").lines().count(), 1);
    let id = ran.iter().find(|r| r.contains("ran, exit 0")).and_then(|r| r.split(':').next()).expect("it ran once").to_string();
    let replayed = broker_exec::decide(&Snapshot { queued: vec![broker_exec::Entry { id: id.clone(), bound_ms: 300_000 }], ..Snapshot::default() });
    let out = w.effects().run(&replayed[0].key, &replayed[0]).unwrap();
    assert!(out.contains("already ran"), "a restart replays the effect and the ledger answers: {out}");
    assert_eq!(marker(&w, "ran.count").lines().count(), 1, "the program did not run twice");
}

#[test_id(tmvlyfb)]
#[test]
fn a_restart_while_a_request_is_running_leaves_it_to_its_runner_and_the_client_gets_that_runners_answer() {
    let w = world();
    w.stub("groomer", &who_ran_me(&w, "swept"));
    let e = w.effects();
    let client = w.command("work").env("SPIRA_RUNE", "groomer").args(["groom", "sweep"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("work runs");
    let spec = runtime::EffectSpec::probe(broker_exec::OBSERVE, "", 10_000);
    let snap = loop {
        let snap: Snapshot = serde_json::from_str(&e.run(&spec.key, &spec).unwrap()).unwrap();
        if !snap.queued.is_empty() {
            break snap;
        }
        std::thread::sleep(Duration::from_millis(10)); // clock-safe: polls the service's queue, the recording being the signal waited on
    };
    let id = snap.queued[0].id.clone();
    let lc = spira_store::lc_call::LcClient::socket(&w.socket);
    lc.answer(&["work-request", "take", &id]).expect("a runner of the old scheduler takes it");
    let restarted = w.effects();
    let again = broker_exec::decide(&snap);
    let out = restarted.run(&again[0].key, &again[0]).unwrap();
    assert!(out.contains("not queued"), "the new scheduler finds it taken: {out}");
    assert_eq!(marker(&w, "ran.count"), "", "the program has not run: the new scheduler did not run it a second time");
    lc.call_stdin(&["work-request", "finish", &id, "--code", "0"], "from the old runner").unwrap();
    let out = client.wait_with_output().unwrap();
    assert_eq!((out.status.code(), String::from_utf8_lossy(&out.stdout).trim()), (Some(0), "from the old runner"));
}
