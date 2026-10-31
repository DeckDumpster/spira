//! `spira-runtime serve` under production conditions: the real binary, config from the shipped
//! registry, a private `dolt sql-server` built the way install builds the lifecycle store
//! (schema, migrations, grants.sql), and `spira-lc serve` logged in as `spira_lc` — never root.
//! The service must still be alive after twenty-odd beats and several round-duty ticks, with the
//! heartbeat URL pinged, the watchdog kicked inside its period and the journal left to what
//! changed: a quiet poll writes nothing to it.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

const RW: &str = "soakRwSecretK4p9TqZ";
const RO: &str = "soakRoSecretW2m8ZcY";
const WATCHDOG_USEC: u64 = 3_000_000;
const SOAK: Duration = Duration::from_secs(25);
const FORBIDDEN: &[&str] = &["journal:", "cannot tell", "refusing"];
const OPEN: &str = "r-auto-691-1029T031850";
const ABANDONED: &str = "r-auto-690-1029T020000";

#[derive(Clone, Copy)]
enum Fifo {
    Real,
    Stalled(Instant),
    Restored,
}

struct Proc(Child);
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spira_lc_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let p = exe.parent().unwrap().parent().unwrap().join("spira-lc");
    assert!(p.is_file(), "{} is not built: the soak drives the real spira-lc, built in the same cargo invocation", p.display());
    p
}

fn tail(path: &Path, max: usize) -> String {
    let text = fs::read_to_string(path).unwrap_or_else(|e| format!("<{}: {e}>", path.display()));
    let start = text.len().saturating_sub(max);
    let start = (start..text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
    text[start..].to_string()
}

struct Store {
    _server: Proc,
    _serve: Proc,
    serve_log: PathBuf,
}

/// The lifecycle store as production has it: install's steps (release::lifecycle_store::steps —
/// schema, migrations, grants.sql with both credentials substituted, migrations again for the
/// content grants) applied as root, then `spira-lc serve` with the `spira_lc` credential.
fn store(root: &Path, toml: &Path, socket: &Path, seed: &str) -> Store {
    assert!(Command::new("dolt").arg("version").output().is_ok(), "dolt must be on PATH: a soak that skips proves nothing"); // STORE_DOOR_CONSIDERED="the soak builds its own private store; spira-lc cannot start the server it talks to"
    let bin = spira_lc_bin();
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let empty = root.join("empty-pw");
    fs::write(&empty, "").unwrap();
    let one = root.join("one.sql");
    fs::write(&one, "SELECT 1;\n").unwrap();
    for _ in 0..4 {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let data = root.join("data");
        let _ = fs::remove_dir_all(&data);
        fs::create_dir_all(&data).unwrap();
        fs::write(
            root.join("server.yaml"),
            format!("log_level: warning\nlistener:\n  port: {port}\n  max_connections: 50\ndata_dir: \"{}\"\nbehavior:\n  dolt_transaction_commit: false\n  event_scheduler: \"OFF\"\n", data.display()),
        )
        .unwrap();
        let base: Vec<(String, String)> = vec![
            ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".into(), root.join("home").display().to_string()),
            ("SPIRA_HOME".into(), home.display().to_string()),
            ("SPIRA_TOML".into(), toml.display().to_string()),
            ("SPIRA_LC_HOST".into(), "127.0.0.1".into()),
            ("SPIRA_LC_PORT".into(), port.to_string()),
            ("SPIRA_LC_DATA_DIR".into(), root.display().to_string()),
        ];
        let lc = |args: &[String], env: &[(String, String)]| -> (i32, String) {
            // batch-job: one spira-lc admin call against the test's private server; bounded by the test timeout.
            let o = Command::new(&bin).args(args).env_clear().envs(base.iter().chain(env).map(|(k, v)| (k, v))).stdin(Stdio::null()).output().expect("spira-lc runs");
            (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
        };
        // batch-job: the test's own disposable sql-server, killed by Proc::drop.
        let mut server = Proc(Command::new("dolt").args(["sql-server", "--config"]).arg(root.join("server.yaml")).current_dir(root).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap()); // STORE_DOOR_CONSIDERED="the test's own disposable server, not the lifecycle store"
        let admin: Vec<(String, String)> = [
            ("SPIRA_LC_USER", "root"),
            ("SPIRA_LC_PASSWORD_FILE", empty.to_str().unwrap()),
            ("SPIRA_LC_PASSWORD", ""),
            ("SPIRA_LC_SOCKET", "/nonexistent/spira-soak-admin-never-forwards"),
            ("SPIRA_LC_ADMIN_USER", "root"),
            ("SPIRA_LC_ADMIN_PASSWORD_FILE", empty.to_str().unwrap()),
            ("SPIRA_LC_ADMIN_PASSWORD", ""),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut up = false;
        while Instant::now() < deadline { // clock-safe: caps the wait for the server to accept a login
            if server.0.try_wait().ok().flatten().is_some() {
                break;
            }
            if lc(&["admin-apply-ddl".into(), one.display().to_string()], &admin).0 == 0 {
                up = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(200)); // clock-safe: poll interval inside the readiness cap
        }
        if !up {
            let _ = server.0.kill();
            continue;
        }
        let lifecycle = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lifecycle");
        let grants = root.join("grants.sql");
        fs::write(&grants, fs::read_to_string(lifecycle.join("grants.sql")).unwrap().replace("@SPIRA_LC_PASSWORD@", RW).replace("@SPIRA_LC_RO_PASSWORD@", RO)).unwrap();
        let rows = root.join("seed.sql");
        fs::write(&rows, seed).unwrap();
        let steps = release::lifecycle_store::steps(&lifecycle).expect("install's steps for the shipped lifecycle dir");
        let mut plan: Vec<(&str, PathBuf)> = steps
            .into_iter()
            .map(|s| match s {
                release::lifecycle_store::Step::Schema(p) => ("admin-apply-ddl", p),
                release::lifecycle_store::Step::Migrations(p) | release::lifecycle_store::Step::ContentGrants(p) => ("admin-migrate", p),
                release::lifecycle_store::Step::Grants => ("admin-apply-ddl", grants.clone()),
            })
            .collect();
        plan.push(("admin-apply-ddl", rows));
        for (verb, path) in plan {
            let (rc, out) = lc(&[verb.into(), path.display().to_string()], &admin);
            assert_eq!(rc, 0, "the lifecycle store does not build the way install builds it: {verb} {}: {out}", path.display());
        }

        let serve_log = root.join("spira-lc-serve.log");
        let log = fs::File::create(&serve_log).unwrap();
        // batch-job: the test's own spira-lc serve, killed by Proc::drop.
        let serve = Proc(
            Command::new(&bin)
                .arg("serve")
                .env_clear()
                .envs(base.iter().map(|(k, v)| (k, v)))
                .env("SPIRA_LC_USER", "spira_lc")
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        let s = Store { _server: server, _serve: serve, serve_log };
        while Instant::now() < deadline && runtime::socket::lc_call(socket, &["journal", "cursor"]).is_err() { // clock-safe: caps the wait for serve to answer
            std::thread::sleep(Duration::from_millis(100)); // clock-safe: poll interval inside that cap
        }
        if let Err(e) = runtime::socket::lc_call(socket, &["journal", "cursor"]) {
            panic!("spira-lc serve never answered a journal call as spira_lc: {e}\n--- serve log ---\n{}", tail(&s.serve_log, 4000));
        }
        return s;
    }
    panic!("a fresh dolt server must start with root passwordless (the positive control)");
}

fn heartbeat_sink() -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/ping", l.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            let mut buf = [0u8; 1024];
            let _ = c.read(&mut buf);
            h.fetch_add(1, Ordering::SeqCst);
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    (url, hits)
}

type Notes = Arc<Mutex<Vec<(Instant, String)>>>;

fn notify_socket(path: &Path) -> Notes {
    let rx = UnixDatagram::bind(path).unwrap();
    let notes: Notes = Arc::default();
    let n = notes.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 256];
        while let Ok(len) = rx.recv(&mut buf) {
            n.lock().unwrap().push((Instant::now(), String::from_utf8_lossy(&buf[..len]).to_string()));
        }
    });
    notes
}

/// Every journal event, read as the runtime reads it: through spira-lc's socket as `spira_lc`.
fn journal(socket: &Path) -> Result<Vec<Value>, String> {
    let mut all = vec![];
    loop {
        let from = all.len().to_string();
        let page: Vec<(u64, Value)> = serde_json::from_str(&runtime::socket::lc_call(socket, &["journal", "read", &from, "1000"]).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let n = page.len();
        all.extend(page.into_iter().map(|(_, ev)| ev));
        if n < 1000 {
            return Ok(all);
        }
    }
}

#[test]
fn e2e_spira_runtime_serves_for_twenty_beats_against_the_production_store() {
    let t = testkit::TempDir::new("runtime-soak");
    let root = t.path().to_path_buf();
    let run = root.join("run");
    for d in ["home", "repo", "run/queue/spira", "run/rounds", "run/watchd"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    fs::write(root.join("rows"), format!("spira | {} | queue.local | local/main | |\n", root.join("repo").display())).unwrap();
    fs::write(
        run.join("queue").join("spira").join("round"), // the fixture writes the record the service never reads
        format!("batch_id={OPEN}\nrepo=spira\nhead=d7a5c242e3e62aa1dc1ffe5af88125e4635eb8e4\nbase=ffacdee2ae397f1918922f8eea231bb5ddf8b4aa\nmembers=sp-soak.1:a57ca1ca8b41391f1b932ab705362d43084c1a49\nworktree={}\nactor=ryan\nopened=1793243936\n", root.join("repo").display()),
    )
    .unwrap();
    fs::write(run.join("queue/spira/round.r-auto-690-1029T020000"), format!("batch_id={ABANDONED}\nrepo=spira\nopened=1793240000\n")).unwrap();
    fs::write(run.join("queue/spira/round-seq"), "874\n").unwrap();
    fs::write(run.join("rounds/300.result"), "").unwrap();
    let cred = root.join("spira-lc.credential");
    fs::write(&cred, RW).unwrap();
    let socket = root.join("lc.sock");
    let inbox = run.join("watchd/concierge-inbox.log");
    let wiki = root.join("wiki");
    fs::create_dir_all(&wiki).unwrap();
    // batch-job: one git init on the fixture's own wiki; brain-dirty alarms on a wiki git does not own.
    assert!(Command::new("git").arg("init").arg("-q").arg(&wiki).env_clear().env("PATH", std::env::var("PATH").unwrap_or_default()).status().unwrap().success());
    let (url, pings) = heartbeat_sink();
    let declare = [
        ("SPIRA_WIKI", wiki.display().to_string()),
        ("SPIRA_RUN", run.display().to_string()),
        ("SPIRA_LC_SOCKET", socket.display().to_string()),
        ("SPIRA_LC_PASSWORD_FILE", cred.display().to_string()),
        ("SPIRA_RUNTIME_HEARTBEAT_SECS", "1".into()),
        ("SPIRA_RUNTIME_ROUND_DUTY_SECS", "1".into()),
        ("SPIRA_RUNTIME_HEARTBEAT_URL", url),
        ("SPIRA_QUEUE_DIR", run.join("queue").display().to_string()),
        ("SPIRA_REPO_MAP", root.join("rows").display().to_string()),
        ("SPIRA_HOME_REPO", "spira".into()),
        ("SPIRA_CONCIERGE_INBOX", inbox.display().to_string()),
    ];
    let declare: Vec<(&str, &str)> = declare.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let env = spira_config::process::fixture_metrics_env(&root, "", &declare);
    let toml = PathBuf::from(&env.iter().find(|(k, _)| *k == "SPIRA_TOML").unwrap().1);
    let started = Instant::now();
    let seed = format!(
        "USE spira_lifecycle;\nINSERT INTO batch (batch_id, repo, state, head, base, pass, phase, version, opened_at) VALUES ('{OPEN}', 'spira', 'CI_RUNNING', 'd7a5c242e3e62aa1dc1ffe5af88125e4635eb8e4', 'ffacdee2ae397f1918922f8eea231bb5ddf8b4aa', 1, 'suites', 3, 1793243936), ('{ABANDONED}', 'spira', 'ABANDONED', NULL, NULL, 1, NULL, 4, 1793240000);\n"
    );
    let store = store(&root, &toml, &socket, &seed);
    let setup = started.elapsed();

    let notify = root.join("notify.sock");
    let notes = notify_socket(&notify);
    let stderr_path = root.join("spira-runtime.log");
    let log = fs::File::create(&stderr_path).unwrap();
    // batch-job: the binary under test, killed by Proc::drop.
    let mut rt = Proc(
        Command::new(env!("CARGO_BIN_EXE_spira-runtime"))
            .arg("serve")
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", root.join("home"))
            .envs(env)
            .env("NOTIFY_SOCKET", &notify)
            .env("WATCHDOG_USEC", WATCHDOG_USEC.to_string())
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let diag = |why: &str| -> String {
        let otel: String = fs::read_dir(spira_otel::dir_of(&run))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| format!("--- {} ---\n{}\n", e.path().display(), tail(&e.path(), 3000)))
            .collect();
        format!(
            "{why}\n--- spira-runtime stderr/stdout ---\n{}\n--- spira-lc serve log ---\n{}\n--- otel (tail) ---\n{otel}--- notify ---\n{:?}\n--- inbox ---\n{}",
            tail(&stderr_path, 8000),
            tail(&store.serve_log, 4000),
            notes.lock().unwrap().iter().map(|(_, m)| m.clone()).collect::<Vec<_>>(),
            tail(&inbox, 2000)
        )
    };

    let begun = Instant::now();
    let mut appended = false;
    let log_file = spira_otel::dir_of(&run).join(format!("spira-runtime.logs.{}.jsonl", spira_otel::utc_date(std::time::SystemTime::now())));
    let parked = log_file.with_extension("parked");
    let mut fifo = Fifo::Real;
    while begun.elapsed() < SOAK { // clock-safe: the soak's wall time is the thing under test
        if let Some(st) = rt.0.try_wait().unwrap() {
            panic!("{}", diag(&format!("spira-runtime exited after {:?} (setup took {setup:?}): {st}", begun.elapsed())));
        }
        if !appended && begun.elapsed() > SOAK / 2 { // clock-safe: the append lands mid-soak by design
            fs::OpenOptions::new().append(true).open(run.join("rounds/300.result")).unwrap().write_all(b"red sp-soak\n").unwrap();
            appended = true;
        }
        match fifo {
            Fifo::Real if begun.elapsed() > SOAK / 5 => { // clock-safe: the stall phase is timed by design
                // a log file that is a pipe nobody reads: the writer thread's open blocks
                fs::rename(&log_file, &parked).unwrap();
                let c = std::ffi::CString::new(log_file.to_str().unwrap()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "{}", std::io::Error::last_os_error());
                fifo = Fifo::Stalled(Instant::now());
            }
            Fifo::Stalled(since) if since.elapsed() > Duration::from_secs(10) => { // clock-safe: the stall lasts 10 s by design
                let _release = fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&log_file).unwrap();
                fs::remove_file(&log_file).unwrap();
                fs::rename(&parked, &log_file).unwrap();
                fifo = Fifo::Restored;
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(200)); // clock-safe: poll interval for the process staying alive
    }
    if let Some(st) = rt.0.try_wait().unwrap() {
        panic!("{}", diag(&format!("spira-runtime exited at the end of the soak: {st}")));
    }
    let _ = rt.0.kill();
    let _ = rt.0.wait();

    let events = journal(&socket).unwrap_or_else(|e| panic!("{}", diag(&format!("the journal does not read back through spira-lc: {e}"))));
    let count = |stream: &str, tag: &str| events.iter().filter(|e| e["stream"] == stream && e["kind"]["tag"] == tag).count();
    let polls = events.iter().filter(|e| e["kind"]["tag"] == "effect.requested" && e["kind"]["body"].as_str().is_some_and(|b| b.contains("observe") || b.contains("heartbeat.ping"))).count();
    let summary = format!("setup {setup:?}; {} events, {polls} observe or ping requests, {} pings", events.len(), pings.load(Ordering::SeqCst));
    eprintln!("soak: {summary}");
    assert_eq!(polls, 0, "{}", diag(&format!("a poll or a ping was journaled: {summary}")));
    assert_eq!(count("runtime.heartbeat", "heartbeat"), 0, "{}", diag(&format!("a beat was journaled: {summary}")));
    assert!(count("watch.round-duty", "round-duty.state") >= 1, "{}", diag(&format!("the first observation of the world is journaled as state: {summary}")));
    assert!(pings.load(Ordering::SeqCst) >= 10, "{}", diag(&format!("the heartbeat URL was pinged fewer than 10 times: {summary}")));
    for stream in ["runtime.heartbeat", "watch.round-duty"] {
        let p = runtime::socket::lc_call(&socket, &["journal", "poisoned", stream]).map_err(|e| e.to_string());
        assert_eq!(p.as_deref(), Ok("null"), "{}", diag(&format!("{stream} was quarantined")));
    }

    let notes = notes.lock().unwrap().clone();
    assert_eq!(notes.first().map(|(_, m)| m.as_str()), Some("READY=1"), "{}", diag("the service never told systemd it was ready"));
    let kicks: Vec<Instant> = notes.iter().filter(|(_, m)| m == "WATCHDOG=1").map(|(t, _)| *t).collect();
    assert!(kicks.len() >= 5, "{}", diag(&format!("{} watchdog kicks in {SOAK:?}", kicks.len())));
    let period = Duration::from_micros(WATCHDOG_USEC);
    let worst = kicks.windows(2).map(|w| w[1] - w[0]).max().unwrap_or_default();
    assert!(worst < period, "{}", diag(&format!("a {worst:?} gap between watchdog kicks: systemd would have restarted the service (WatchdogSec {period:?})")));

    assert!(matches!(fifo, Fifo::Restored), "{}", diag("the log-stall phase never completed"));
    let out = fs::read_to_string(&stderr_path).unwrap_or_default();
    let bad: Vec<&str> = out.lines().filter(|l| FORBIDDEN.iter().any(|f| l.contains(f))).collect();
    assert!(bad.is_empty(), "{}", diag(&format!("the runtime reported faults: {bad:?}")));
    let said = fs::read_to_string(&inbox).unwrap_or_default();
    assert!(said.contains("ROUND RESULT 300: red sp-soak"), "{}", diag("round-duty never relayed the result line to the inbox"));
    assert!(!said.contains("BRAIN DIRTY"), "{}", diag("brain-dirty alarmed on a clean git wiki"));
    assert!(!said.contains("ROUND STATE UNKNOWN"), "{}", diag("round-duty called an open round's state unknown"));
}
