//! For one timeline of review state, `cockpit-remote watch` and the view handler flip the cockpit
//! to the same windows in the same order and leave the same dismissals. The script is the oracle
//! and runs unmodified against a stubbed `hunk` and `tmux`; the handler asks an HTTP stub of the
//! hunk daemon that verifies every signature it is shown, and launches a stubbed `tmux`.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ring::digest::{digest, SHA256};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use runtime::host::EffectRunner;
use runtime::ports::{Clock, VirtualClock};
use runtime::{Engine, Executor, Journal, MemJournal, Outcome};
use serde_json::{json, Value};
use spira_runtime::hunk::{b64url, b64url_decode, canonical};
use spira_runtime::watchers::view::{self, View};
use spira_runtime::world::World;
use spira_runtime::Effects;

const FORGET: u64 = 99;
const A: &str = "aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa";
const B: &str = "bbbbbbbb-2222-4222-8222-bbbbbbbbbbbb";
const SECOND: u64 = 1000;
const PASS: u64 = 9;

fn write_exec(path: &Path, text: &str) {
    testkit::write_exe(path, text);
}

/// A tmux that knows two linked windows and records which cockpit window is selected.
fn tmux_stub(path: &Path, log: &Path) {
    write_exec(
        path,
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n has-session) [ -e \"{end}\" ] && exit 1; exit 0;;\n display-message) case \"$4\" in =brain:0) echo @1;; =hunk:0) echo @2;; esac;;\n list-windows) printf '0 @1\\n1 @2\\n';;\n select-window) echo \"$3\" >>\"{log}\";;\nesac\n[ -n \"$TMUX\" ] && echo \"TMUX was set\" >>\"{log}.leak\"\nexit 0\n",
            end = path.parent().unwrap().parent().unwrap().join("end").display(),
            log = log.display()
        ),
    );
}

struct Daemon {
    port: u16,
    live: Arc<Mutex<Option<Vec<String>>>>,
}

/// (method, path, lower-cased headers, body)
type Req = (String, String, HashMap<String, String>, Vec<u8>);

fn request(stream: &mut std::net::TcpStream) -> Option<Req> {
    let mut raw = vec![];
    let mut buf = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
    let headers: HashMap<String, String> = lines.filter_map(|l| l.split_once(": ")).map(|(k, v)| (k.to_lowercase(), v.to_string())).collect();
    let want: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    while raw.len() < head_end + want {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        raw.extend_from_slice(&buf[..n]);
    }
    Some((method, path, headers, raw[head_end..head_end + want].to_vec()))
}

fn reply(stream: &mut std::net::TcpStream, status: u16, body: &Value) {
    let body = body.to_string();
    let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
}

/// The daemon's side of the signed session, refusing anything the caller key did not sign.
fn daemon(caller_public: Vec<u8>, key_id: &'static str, grant_id: &'static str) -> Daemon {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let live: Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(Some(vec![])));
    let shared = live.clone();
    std::thread::spawn(move || {
        let verify = |msg: &str, sig: &str| b64url_decode(sig).is_some_and(|s| UnparsedPublicKey::new(&ED25519, &caller_public).verify(msg.as_bytes(), &s).is_ok());
        let endpoint = format!("http://127.0.0.1:{port}/session-auth/challenge");
        let mut challenges: HashMap<String, String> = HashMap::new();
        let mut sessions: HashMap<String, (String, String, String)> = HashMap::new();
        for mut stream in listener.incoming().flatten() {
            let Some((method, path, headers, body)) = request(&mut stream) else { continue };
            let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            match (method.as_str(), path.as_str()) {
                ("POST", "/session-auth/challenge") => {
                    let ok = json["role"] == "caller" && json["appId"] == "dev.hunk" && json["endpoint"] == endpoint.as_str() && json["keyId"] == key_id && json["grantId"] == grant_id && json["proposal"] == json!({"brokerRevision": 1, "appRevision": 16, "features": []});
                    let nonce = json["initiatorNonce"].as_str().unwrap_or_default();
                    if !ok || nonce.is_empty() {
                        reply(&mut stream, 401, &json!({"error": "bad hello"}));
                        continue;
                    }
                    let id = format!("c{}", challenges.len());
                    let transcript = canonical(&json!({"appId": "dev.hunk", "domain": "dev.hunk.session-broker.v1/caller-hello", "endpoint": endpoint, "generation": "g1", "grantId": grant_id, "initiatorNonce": nonce, "keyId": key_id, "proposal": json["proposal"], "responderNonce": "r1"}));
                    challenges.insert(id.clone(), transcript);
                    reply(&mut stream, 200, &json!({"challengeId": id, "generation": "g1", "responderNonce": "r1", "expiresAt": 0, "daemonKeyId": "d", "daemonSignature": ""}));
                }
                ("POST", "/session-auth/proof") => {
                    let Some(transcript) = json["challengeId"].as_str().and_then(|c| challenges.remove(c)) else { reply(&mut stream, 401, &json!({})); continue };
                    if !verify(&transcript, json["signature"].as_str().unwrap_or_default()) {
                        reply(&mut stream, 401, &json!({"error": "bad proof"}));
                        continue;
                    }
                    let id = format!("s{}", sessions.len());
                    sessions.insert(id.clone(), ("g1".into(), b64url(digest(&SHA256, transcript.as_bytes()).as_ref()), "7".into()));
                    reply(&mut stream, 200, &json!({"callerSessionId": id, "initialSequence": "7"}));
                }
                ("POST", "/session-api") => {
                    let header = |k: &str| headers.get(k).cloned().unwrap_or_default();
                    let Some((generation, hello_hash, sequence)) = sessions.get(&header("x-session-broker-caller-session")) else { reply(&mut stream, 401, &json!({})); continue };
                    let signed = canonical(&json!({
                        "appId": "dev.hunk", "bodyDigest": b64url(digest(&SHA256, &body).as_ref()), "callerSessionId": header("x-session-broker-caller-session"),
                        "domain": "dev.hunk.session-broker.v1/caller-request", "generation": generation, "grantId": grant_id, "helloTranscriptHash": hello_hash,
                        "keyId": key_id, "method": "POST", "requestId": header("x-session-broker-request-id"), "sequence": header("x-session-broker-sequence"), "target": "/session-api",
                    }));
                    if header("x-session-broker-sequence") != *sequence || !verify(&signed, &header("x-session-broker-signature")) || json != json!({"action": "list"}) {
                        reply(&mut stream, 401, &json!({"error": "bad request signature"}));
                        continue;
                    }
                    match shared.lock().unwrap().clone() {
                        Some(ids) => reply(&mut stream, 200, &json!({"body": {"sessions": ids.iter().map(|i| json!({"sessionId": i, "title": "R1"})).collect::<Vec<_>>()}, "authentication": {}})),
                        None => reply(&mut stream, 503, &json!({"error": "down"})),
                    }
                }
                _ => reply(&mut stream, 404, &json!({})),
            }
        }
    });
    Daemon { port, live }
}

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    dir: testkit::TempDir,
    daemon: Daemon,
}

impl Fixture {
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn build() -> Fixture {
        let dir = testkit::TempDir::new("view-parity");
        let root = dir.path();
        for d in ["bin", "tmp", "old-state", "new-state", "xdg/hunk-mcp/security-v1", "new-bin"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        fs::write(root.join("xdg/hunk-mcp/security-v1/caller.json"), json!({"keyId": "k1", "privateKey": b64url(pkcs8.as_ref()), "grant": {"grantId": "grant1"}}).to_string()).unwrap();
        let daemon = daemon(pair.public_key().as_ref().to_vec(), "k1", "grant1");
        fs::write(root.join(format!("xdg/hunk-mcp/daemon-127-0-0-1-{}.json", daemon.port)), json!({"host": "127.0.0.1", "port": daemon.port}).to_string()).unwrap();
        write_exec(&root.join("bin/hunk"), "#!/bin/sh\n[ -e \"$0.down\" ] && exit 3\nwhile read -r id; do echo \"$id R1 design\"; done <\"$0.live\"\n");
        write_exec(&root.join("bin/date"), "#!/bin/sh\ncat \"$(dirname \"$0\")/../now\"\n");
        write_exec(&root.join("bin/sleep"), "#!/bin/sh\nd=\"$(dirname \"$0\")/..\"\nn=$(( $(cat \"$d/slept\" 2>/dev/null || echo 0) + 1 ))\necho $n >\"$d/slept.tmp\"; mv \"$d/slept.tmp\" \"$d/slept\"\nwhile [ ! -e \"$d/go.$n\" ]; do /bin/sleep 0.01; done\n");
        tmux_stub(&root.join("bin/tmux"), &root.join("old-flips"));
        tmux_stub(&root.join("new-bin/tmux"), &root.join("new-flips"));
        fs::write(root.join("bin/hunk.live"), "").unwrap();
        Fixture { dir, daemon }
    }

    fn set_live(&self, ids: &[&str]) {
        fs::write(self.p("bin/hunk.live"), ids.iter().map(|i| format!("{i}\n")).collect::<String>()).unwrap();
        *self.daemon.live.lock().unwrap() = Some(ids.iter().map(|i| i.to_string()).collect());
    }

    fn set_down(&self, down: bool) {
        if down {
            fs::write(self.p("bin/hunk.down"), "").unwrap();
        } else {
            let _ = fs::remove_file(self.p("bin/hunk.down"));
        }
        let live = if down { None } else { Some(fs::read_to_string(self.p("bin/hunk.live")).unwrap().lines().map(str::to_string).collect()) };
        *self.daemon.live.lock().unwrap() = live;
    }

    fn dismiss(&self, id: &str) {
        for d in ["old-state", "new-state"] {
            fs::write(self.p(d).join("dismissed"), format!("{id}\n")).unwrap();
        }
    }

    fn flips(&self, side: &str) -> Vec<String> {
        fs::read_to_string(self.p(&format!("{side}-flips"))).unwrap_or_default().lines().map(str::to_string).collect()
    }

    /// The absence marks, in order, with a missing file the same as an empty one.
    fn absent(&self, side: &str) -> Vec<String> {
        let mut marks: Vec<String> = fs::read_to_string(self.p(&format!("{side}-state/dismissed.absent"))).unwrap_or_default().lines().map(str::to_string).collect();
        marks.sort();
        marks
    }

    fn dismissed(&self, side: &str) -> String {
        fs::read_to_string(self.p(&format!("{side}-state/dismissed"))).unwrap_or_default()
    }
}

/// The script's `watch` loop, one pass per `step`, with `sleep` as the gate between passes.
struct Oracle<'a> {
    f: &'a Fixture,
    _child: Killed,
}

impl Oracle<'_> {
    fn start(f: &Fixture) -> Oracle<'_> {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../cockpit/remote/cockpit-remote");
        let child = Command::new("bash")
            .arg(&script)
            .arg("watch")
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", f.p("bin").display()))
            .env("HOME", f.dir.path())
            .env("TMPDIR", f.p("tmp"))
            .env("HUNK", f.p("bin/hunk"))
            .env("COCKPIT_HUNK_NOFENCE", "1")
            .env("COCKPIT_STATE", f.p("old-state"))
            .env("COCKPIT_FORGET_SECS", FORGET.to_string())
            .env("INVOCATION_ID", "unit")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let o = Oracle { f, _child: Killed(child) };
        o.wait_for(1);
        o
    }

    fn wait_for(&self, passes: u32) {
        let until = Instant::now() + Duration::from_secs(20); // clock-safe: a cap on the wait for the script's pass marker
        while fs::read_to_string(self.f.p("slept")).ok().and_then(|s| s.trim().parse::<u32>().ok()) < Some(passes) {
            assert!(Instant::now() < until, "cockpit-remote watch did not finish pass {passes}"); // clock-safe: the cap above
            std::thread::sleep(Duration::from_millis(5)); // clock-safe: polling the pass marker file the stubbed sleep writes
        }
    }

    fn step(&self, n: u32) {
        fs::write(self.f.p(&format!("go.{n}")), "").unwrap();
        self.wait_for(n + 1);
    }
}

struct Handler {
    e: Engine,
    clock: VirtualClock,
    fx: Effects,
}

impl Handler {
    fn start(f: &Fixture, now: u64) -> Handler {
        let world = World { hunk_dir: f.p("xdg/hunk-mcp"), cockpit_state: f.p("new-state"), cockpit_session: "cockpit".into(), tmux: f.p("new-bin/tmux"), ..World::default() };
        let clock = VirtualClock::new(now * SECOND);
        let reg = View { interval_ms: PASS * SECOND, forget_secs: FORGET, enabled: true }.registration();
        let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), vec![reg], 0).unwrap();
        let (_, stream, kind) = View::boot();
        e.start(stream, kind);
        Handler { e, clock, fx: Effects { world, ..Effects::default() } }
    }

    fn settle(&mut self) {
        loop {
            self.e.run_until_idle().unwrap();
            let pending = self.e.pending_effects();
            if pending.is_empty() {
                return;
            }
            for p in pending {
                let out = match self.fx.run(&p.key, &p.spec) {
                    Ok(d) => Outcome::Succeeded(d),
                    Err(d) => Outcome::Failed(d),
                };
                self.e.complete(&p.key, out).unwrap();
            }
        }
    }

    /// Runs the one tick that falls due in each pass the script makes, so both sides read the world at the same moments.
    fn step(&mut self, now: u64) {
        while self.clock.now_ms() < now * SECOND {
            self.clock.advance(PASS * SECOND);
            self.settle();
        }
    }
}

#[test]
fn the_handler_flips_and_forgets_exactly_as_cockpit_remote_watch_does() {
    let f = Fixture::build();
    let t0 = 1_800_000_000;
    let tick = |n: u64| t0 + n * PASS;
    fs::write(f.p("now"), t0.to_string()).unwrap();
    let oracle = Oracle::start(&f);
    let mut handler = Handler::start(&f, t0);
    handler.settle();

    let mut n = 0;
    let mut pass = |f: &Fixture, handler: &mut Handler, at: u64| {
        n += 1;
        fs::write(f.p("now"), at.to_string()).unwrap();
        oracle.step(n);
        handler.step(at);
        assert_eq!(f.dismissed("new"), f.dismissed("old"), "dismissals diverged at pass {n}");
        assert_eq!(f.absent("new"), f.absent("old"), "absence marks diverged at pass {n}");
        assert_eq!(f.flips("new"), f.flips("old"), "flips diverged at pass {n}");
    };

    pass(&f, &mut handler, tick(1));
    assert!(f.flips("old").is_empty(), "the first reading only seeds");

    f.set_live(&[A]);
    pass(&f, &mut handler, tick(2));
    assert_eq!(f.flips("old"), ["=cockpit:1"], "a live review shows hunk");

    f.dismiss(A);
    pass(&f, &mut handler, tick(3));
    assert_eq!(f.flips("old"), ["=cockpit:1", "=cockpit:0"], "dismissed, it shows brain");

    f.set_live(&[]);
    pass(&f, &mut handler, tick(4));
    assert_eq!(f.absent("old").len(), 1, "control: the marks are compared when there is something to compare");
    pass(&f, &mut handler, tick(4) + FORGET - PASS);
    assert_eq!(f.dismissed("old"), format!("{A}\n"), "absent inside the window keeps the dismissal");
    pass(&f, &mut handler, tick(4) + FORGET + PASS);
    assert_eq!(f.dismissed("old"), "", "absent past the window forgets it");
    assert!(f.absent("old").is_empty());

    f.set_live(&[B]);
    pass(&f, &mut handler, tick(4) + FORGET + 2 * PASS);
    assert_eq!(f.flips("old"), ["=cockpit:1", "=cockpit:0", "=cockpit:1"], "a new review shows hunk");

    f.dismiss(B);
    f.set_down(true);
    pass(&f, &mut handler, tick(4) + 3 * FORGET);
    assert_eq!(f.dismissed("old"), format!("{B}\n"), "a hunk that did not answer forgets nothing");
    assert_eq!(f.flips("old").len(), 3, "and flips nothing");

    f.set_down(false);
    pass(&f, &mut handler, tick(4) + 3 * FORGET + PASS);
    assert_eq!(f.flips("old"), ["=cockpit:1", "=cockpit:0", "=cockpit:1", "=cockpit:0"]);
    assert!(!f.p("old-flips.leak").exists() && !f.p("new-flips.leak").exists(), "tmux must run with TMUX unset");

    fs::write(f.p("end"), "").unwrap();
    fs::write(f.p(&format!("go.{}", n + 1)), "").unwrap();
}

fn snapshot(live: Option<&[&str]>, dismissed: &[&str]) -> String {
    let live = live.map(|l| l.iter().map(|s| s.to_string()).collect());
    let ids = dismissed.iter().map(|s| s.to_string()).collect();
    serde_json::to_string(&view::Snapshot { seed: false, live, error: None, dismissed: view::Dismissed { ids, absent: Default::default() } }).unwrap()
}

fn engine(journal: Arc<MemJournal>, clock: &VirtualClock, enabled: bool) -> Engine {
    let reg = View { interval_ms: 3 * SECOND, forget_secs: FORGET, enabled }.registration();
    Engine::open(journal, Arc::new(clock.clone()), vec![reg], 0).unwrap()
}

/// One tick: the observe is answered with `reply`; every effect the handler asked for is named.
fn tick(e: &mut Engine, clock: &VirtualClock, reply: &str) -> Vec<String> {
    let mut asked = vec![];
    loop {
        e.run_until_idle().unwrap();
        let pending = e.pending_effects();
        if pending.is_empty() {
            break;
        }
        for p in pending {
            asked.push(p.spec.name.clone());
            let out = if p.spec.name == view::OBSERVE { Outcome::Succeeded(reply.into()) } else { Outcome::Succeeded(String::new()) };
            e.complete(&p.key, out).unwrap();
        }
    }
    clock.advance(3 * SECOND);
    asked
}

#[test]
fn ten_ticks_of_an_unchanged_review_state_journal_nothing_and_flip_nothing() {
    let mem = MemJournal::new();
    let clock = VirtualClock::new(1_800_000_000 * SECOND);
    let mut e = engine(mem.clone(), &clock, true);
    let (_, stream, kind) = View::boot();
    e.start(stream, kind);
    let quiet = snapshot(Some(&[A]), &[]);
    let first = tick(&mut e, &clock, &quiet);
    assert_eq!(first, [view::OBSERVE]);
    let after_first = mem.read_from(0, 1000).unwrap();
    assert_eq!(after_first.iter().filter(|(_, ev)| ev.kind.tag == view::STATE).count(), 1, "the first reading records the state");
    for _ in 0..10 {
        let asked = tick(&mut e, &clock, &quiet);
        assert!(asked.iter().all(|n| n == view::OBSERVE), "an unchanged review state requested {asked:?}");
    }
    assert_eq!(mem.read_from(0, 1000).unwrap().len(), after_first.len(), "ten idle ticks journaled something");
    let changed = tick(&mut e, &clock, &snapshot(Some(&[]), &[]));
    assert!(changed.contains(&view::FLIP.to_string()) && mem.read_from(0, 1000).unwrap().len() > after_first.len(), "control: a real change is journaled and flips");
}

#[test]
fn a_restart_reads_the_view_once_to_seed_it_instead_of_flipping() {
    let mem = MemJournal::new();
    let clock = VirtualClock::new(1_800_000_000 * SECOND);
    let mut e = engine(mem.clone(), &clock, true);
    let (_, stream, kind) = View::boot();
    e.start(stream, kind);
    tick(&mut e, &clock, &snapshot(Some(&[A]), &[]));
    drop(e);

    let mut again = engine(mem.clone(), &clock, true);
    let (_, stream, kind) = View::boot();
    again.start(stream, kind);
    let seeded = serde_json::to_string(&view::Snapshot { seed: true, ..serde_json::from_str(&snapshot(Some(&[]), &[])).unwrap() }).unwrap();
    assert_eq!(tick(&mut again, &clock, &seeded), [view::OBSERVE], "the first reading after a start must not flip");
    assert_eq!(tick(&mut again, &clock, &snapshot(Some(&[A]), &[])), [view::OBSERVE, view::FLIP], "a change after the seed is a transition");
}

#[test]
fn without_a_view_program_the_handler_schedules_nothing() {
    let clock = VirtualClock::new(1_800_000_000 * SECOND);
    let mut e = engine(MemJournal::new(), &clock, false);
    let (_, stream, kind) = View::boot();
    e.start(stream, kind);
    e.run_until_idle().unwrap();
    assert!(e.pending_effects().is_empty() && e.next_timer_at().is_none());
}

#[test]
fn a_hunk_that_answers_nothing_is_a_keep_that_carries_why() {
    let f = Fixture::build();
    f.set_down(true);
    let world = World { hunk_dir: f.p("xdg/hunk-mcp"), cockpit_state: f.p("new-state"), ..World::default() };
    let snap: view::Snapshot = serde_json::from_str(&world.view_snapshot("").unwrap()).unwrap();
    assert!(snap.live.is_none() && snap.error.unwrap().contains("503"), "the reading must be keep, with the error");
    let no_daemon = World { hunk_dir: f.p("nowhere"), ..World::default() };
    assert!(serde_json::from_str::<view::Snapshot>(&no_daemon.view_snapshot("").unwrap()).unwrap().live.is_none());
}

#[test]
fn the_watchers_manifest_no_longer_carries_the_row() {
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/watchers")).unwrap();
    assert!(manifest.lines().any(|l| l.starts_with("mail-deliver|")), "the scan must be able to see a row");
    assert!(!manifest.lines().any(|l| l.trim_start_matches('?').starts_with("view|")), "spira-runtime hosts the view follower");
}
