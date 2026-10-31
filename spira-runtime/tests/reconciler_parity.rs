//! For one fixture world, `reconciler --pass` and the reconciler handler make the same remedy
//! calls, send the same escalation mails and carry the same hysteresis state over three passes.
//! The binary is the oracle: it runs against stub programs on PATH; the handler runs on a virtual
//! clock against a fake user-manager bus, a fake spira-lc socket and an HTTP stub of the forge,
//! all serving the same facts. A second world with every invariant satisfied shows an idle pass
//! journals nothing and launches none of the programs it replaced.
// covers: reconciler/src/*.rs reconciler-engine/src/*.rs spira-runtime/src/reconcile_host.rs spira-runtime/src/watchers/reconciler.rs spira-runtime/src/lib.rs

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use reconciler::{Book, Rules, Seams};
use reconciler_engine::clock::iso_of;
use runtime::event::*;
use runtime::host::EffectRunner;
use runtime::journal::Journal as _;
use runtime::ports::{Clock as _, VirtualClock};
use runtime::{Engine, Executor, MemJournal, Outcome};
use spira_runtime::forge::Forge;
use spira_runtime::watchers::reconciler::{self as handler, Reconciler};
use spira_runtime::world::World;
use spira_runtime::Effects;

const T0: u64 = 1_800_000_000;
const PASS_SECS: u64 = 60;
const ROWS: &str = r#"[{"bead_id":"sp-junk","state":"READY"},{"bead_id":"sp-real","state":"READY"}]"#;
const QUIET_ROWS: &str = r#"[{"bead_id":"sp-real","state":"READY"}]"#;
const NO_ORPHANS: &str = r#"{"orphans":[],"requeued":[],"failed":[]}"#;

#[derive(Clone, Copy, PartialEq)]
enum Scenario {
    /// A dead enabled unit, a fleet gap, disk under its floor, a stranded run, a junk row, an epic edge.
    Broken,
    /// Everything satisfied.
    Quiet,
}

fn debug_dir() -> PathBuf {
    std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_path_buf()
}

// ── the fake user-manager bus ─────────────────────────────────────────────────────────────

fn align(b: &mut Vec<u8>, n: usize) {
    while !b.len().is_multiple_of(n) {
        b.push(0);
    }
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    align(b, 4);
    b.extend_from_slice(&(s.len() as u32).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
    b.push(0);
}

fn put_u32(b: &mut Vec<u8>, v: u32) {
    align(b, 4);
    b.extend_from_slice(&v.to_le_bytes());
}

/// `a(...)`: the elements are written by `each`, which fills one struct.
fn put_structs<T>(items: &[T], each: impl Fn(&mut Vec<u8>, &T)) -> Vec<u8> {
    let mut b = vec![0, 0, 0, 0];
    align(&mut b, 8);
    let start = b.len();
    for i in items {
        align(&mut b, 8);
        each(&mut b, i);
    }
    let len = (b.len() - start) as u32;
    b[..4].copy_from_slice(&len.to_le_bytes());
    b
}

fn method_return(serial: u32, reply_to: u32, sig: &str, body: Vec<u8>) -> Vec<u8> {
    let mut fields = vec![5, 1, b'u', 0];
    fields.extend_from_slice(&reply_to.to_le_bytes());
    align(&mut fields, 8);
    fields.extend_from_slice(&[8, 1, b'g', 0, sig.len() as u8]);
    fields.extend_from_slice(sig.as_bytes());
    fields.push(0);
    let mut m = vec![b'l', 2, 0, 1];
    m.extend_from_slice(&(body.len() as u32).to_le_bytes());
    m.extend_from_slice(&serial.to_le_bytes());
    m.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    m.extend_from_slice(&fields);
    align(&mut m, 8);
    m.extend_from_slice(&body);
    m
}

fn serve_bus(path: &Path, loaded: Vec<(String, String)>, files: Vec<(String, String)>) {
    let listener = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let (loaded, files) = (loaded.clone(), files.clone());
            std::thread::spawn(move || {
                let mut line = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    if s.read_exact(&mut byte).is_err() {
                        return;
                    }
                    line.push(byte[0]);
                    if line.ends_with(b"\r\n") {
                        if line.ends_with(b"BEGIN\r\n") {
                            break;
                        }
                        let _ = s.write_all(b"OK 0123456789abcdef\r\n");
                        line.clear();
                    }
                }
                loop {
                    let mut head = [0u8; 16];
                    if s.read_exact(&mut head).is_err() {
                        return;
                    }
                    let body_len = u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize;
                    let serial = u32::from_le_bytes(head[8..12].try_into().unwrap());
                    let fields_len = u32::from_le_bytes(head[12..16].try_into().unwrap()) as usize;
                    let padded = fields_len.div_ceil(8) * 8;
                    let mut rest = vec![0u8; padded + body_len];
                    if s.read_exact(&mut rest).is_err() {
                        return;
                    }
                    let mut raw = head.to_vec();
                    raw.extend_from_slice(&rest);
                    let msg = unit_run::dbus::decode(&raw).unwrap();
                    let (sig, body) = match msg.member.as_deref() {
                        Some("Hello") => {
                            let mut b = vec![];
                            put_str(&mut b, ":1.1");
                            ("s", b)
                        }
                        Some("ListUnits") => (
                            "a(ssssssouso)",
                            put_structs(&loaded, |b, (name, state)| {
                                for v in [name.as_str(), "", "loaded", state.as_str(), "running", "", "/o"] {
                                    put_str(b, v);
                                }
                                put_u32(b, 0);
                                put_str(b, "");
                                put_str(b, "/");
                            }),
                        ),
                        Some("ListUnitFiles") => (
                            "a(ss)",
                            put_structs(&files, |b, (path, state)| {
                                put_str(b, path);
                                put_str(b, state);
                            }),
                        ),
                        other => panic!("the fake bus has no answer for {other:?}"),
                    };
                    if s.write_all(&method_return(serial, serial, sig, body)).is_err() {
                        return;
                    }
                }
            });
        }
    });
}

// ── the fake spira-lc socket ──────────────────────────────────────────────────────────────

fn lc_answer(scenario: Scenario, tip: &str, argv: &[String]) -> String {
    match (argv[0].as_str(), argv.get(1).map(String::as_str)) {
        ("list", Some("--batches" | "--hold")) => "[]".into(),
        ("list", Some("--state")) => format!(r#"[{{"bead_id":"sp-cert","tip":"{tip}","since":1}}]"#),
        ("list", _) if scenario == Scenario::Broken => ROWS.into(),
        ("list", _) => QUIET_ROWS.into(),
        ("requeue-orphans", _) => NO_ORPHANS.into(),
        _ => String::new(),
    }
}

fn serve_lc(sock: &Path, scenario: Scenario, tip: String) {
    let listener = UnixListener::bind(sock).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let argv: Vec<String> = serde_json::from_str(&line).unwrap();
            let body = lc_answer(scenario, &tip, &argv);
            let mut w = &stream;
            writeln!(w, "{{\"exit_code\": 0, \"len\": {}}}", body.len()).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
    });
}

// ── the forge, over HTTP ──────────────────────────────────────────────────────────────────

fn serve_forge(scenario: Scenario, created: String) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            let mut buf = [0u8; 2048];
            let n = c.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
            let body = match (scenario, path.as_str()) {
                (Scenario::Broken, p) if p.contains("/actions/runs?") => r#"{"workflow_runs":[{"id":55,"status":"queued"}]}"#.to_string(),
                (Scenario::Broken, p) if p.contains("/actions/runners") => r#"{"runners":[]}"#.to_string(),
                (Scenario::Broken, p) if p.ends_with("/actions/runs/55/jobs") => format!(r#"{{"jobs":[{{"name":"j","status":"queued","created_at":"{created}","labels":["self-hosted","ci-r-55-2"]}}]}}"#),
                (_, p) if p.contains("/actions/runs?") => r#"{"workflow_runs":[]}"#.to_string(),
                _ => "{}".to_string(),
            };
            let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
        }
    });
    format!("http://{addr}")
}

// ── the world ─────────────────────────────────────────────────────────────────────────────

/// Counted: a program the handler must not launch on a tick.
const COUNTED: [&str; 6] = ["systemctl", "date", "spira-lc", "df", "forge", "units-install"];

struct Fixture {
    dir: testkit::TempDir,
    scenario: Scenario,
    /// The certified branch's tip.
    tip: String,
}

impl Fixture {
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// Stub programs in `bin/<side>`; each appends one line to `<side>.calls` when it is run, and
    /// the ones that mutate also to `<side>.remedies`.
    fn stubs(&self, side: &str) -> PathBuf {
        let bin = self.p(&format!("bin-{side}"));
        fs::create_dir_all(&bin).unwrap();
        let calls = self.p(&format!("{side}.calls"));
        let remedies = self.p(&format!("{side}.remedies"));
        let omail = self.p("omail");
        let broken = self.scenario == Scenario::Broken;
        let units_active = if broken { "inactive" } else { "active" };
        let unit = if broken { "spira-dead-prod.service" } else { "spira-ok-prod.service" };
        let lc_rows = if broken { ROWS } else { QUIET_ROWS };
        let date_secs = self.p("now.secs");
        let date_iso = self.p("now.iso");
        let tip = &self.tip;
        let stranded = if broken { "echo 'STRANDED run=55 job=\"j\" label=ci-r-55-2 queued=900s runner=none vmid=?'" } else { ":" };
        let epic = if broken { "echo 'sp-child sp-epic'" } else { ":" };
        let rune = |argv: &str| self.p(&format!("config/{}", argv.replace(' ', "_")));
        let stub = |name: &str, body: String| {
            testkit::write_exe(bin.join(name), &format!("#!/bin/sh\necho \"{name} $*\" >> {}\n{body}\n", calls.display()));
        };
        let remedy = |name: &str| format!("echo \"{name} $*\" >> {}", remedies.display());
        stub("systemctl", format!("case \"$2\" in is-enabled) echo enabled;; is-active) echo {units_active};; *) {};; esac", remedy("systemctl")));
        stub("units-install", format!("echo {unit}"));
        stub("date", format!("case \"$2\" in +%s) cat {};; *) cat {};; esac", date_secs.display(), date_iso.display()));
        stub("spira-lc", format!("case \"$1\" in list) case \"$2\" in --batches|--hold) echo '[]';; --state) echo '[{{\"bead_id\":\"sp-cert\",\"tip\":\"{tip}\",\"since\":1}}]';; *) echo '{lc_rows}';; esac;; requeue-orphans) echo '{NO_ORPHANS}';; drop) {};; esac", remedy("spira-lc")));
        stub("spira-config", format!("cat {}/\"$(echo \"$*\" | tr ' ' _)\" 2>/dev/null; :", self.p("config").display()));
        stub("spira-claim", "echo 3".into());
        stub("strand", "echo 0".into());
        stub("queue-helpers", format!("echo 'sp-cert {tip} 1'"));
        stub("queue", remedy("queue"));
        stub("spira-lint", ":".into());
        stub("lifecycle-guard", ":".into());
        stub("forge", format!("case \"$1\" in stranded-runners) {stranded};; workflow-rerun) {};; esac", remedy("forge")));
        stub("bead", format!("case \"$2\" in epic-edges) {epic};; convert) {};; esac", remedy("bead")));
        stub("target-reap", remedy("target-reap"));
        stub("podman", format!("case \"$1\" in info) :;; *) {};; esac", remedy("podman")));
        stub("git", "case \"$*\" in *'rev-parse --abbrev-ref HEAD'*) echo main;; *) exec /usr/bin/git \"$@\";; esac".into());
        stub("bdq", "echo '[{\"id\":\"sp-real\"}]'".into());
        stub("tmux", "exit 1".into());
        stub("df", "echo 'Filesystem 1024-blocks Used Available Capacity Mounted'; echo 'fake 1000 950 50 95% /'".into());
        stub("rebuild", remedy("rebuild"));
        stub("layout", remedy("layout"));
        testkit::write_exe(bin.join("mail"), &format!("#!/bin/sh\nsubject=\"\"\nwhile [ $# -gt 0 ]; do [ \"$1\" = --subject ] && subject=\"$2\"; shift; done\nn=$(ls {} | wc -l)\n{{ printf '%s\\n' \"$subject\"; cat; }} > {}/$n\n", omail.display(), omail.display()));
        let _ = rune;
        bin
    }

    fn build(name: &str, scenario: Scenario) -> Fixture {
        let mut f = Fixture { dir: testkit::TempDir::new(&format!("rcp-{name}")), scenario, tip: String::new() };
        for d in ["run/queue/widgets", "omail", "hmail", "config", "home/runes", "checkout"] {
            fs::create_dir_all(f.p(d)).unwrap();
        }
        f.tip = f.checkout();
        fs::create_dir_all(f.p("home/.git")).unwrap();
        fs::write(f.p("home/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(f.p("run/cockpit.down"), "").unwrap();
        fs::write(f.p("rows"), format!("widgets | {c} | queue.local | main | |\nspira | {c} | queue.local | local/main | |\n", c = f.p("checkout").display())).unwrap();
        if scenario == Scenario::Broken {
            fs::write(f.p("home/runes/builder.rune"), "RUNE_LABELS=ready-work\n").unwrap();
        }
        for (argv, want) in f.config_answers() {
            fs::write(f.p("config").join(argv.replace(' ', "_")), want).unwrap();
        }
        f.set_now(T0);
        f
    }

    /// A real checkout: `main`, the landing ref `local/main`, and the certified branch, which
    /// conflicts with `main` in the broken world and merges clean in the quiet one. Returns its tip.
    fn checkout(&self) -> String {
        let repo = self.p("checkout");
        let git = |args: &[&str]| testkit::git(&repo, args);
        git(&["init", "-q", "-b", "main"]);
        git(&["remote", "add", "origin", "https://github.com/acme/widgets.git"]);
        fs::write(repo.join("keep"), "x\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "base"]);
        git(&["checkout", "-q", "-b", "spira/sp-cert"]);
        if self.scenario == Scenario::Broken {
            fs::write(repo.join("keep"), "y\n").unwrap();
        } else {
            fs::write(repo.join("added"), "y\n").unwrap();
        }
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "cert"]);
        let tip = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-q", "main"]);
        if self.scenario == Scenario::Broken {
            fs::write(repo.join("keep"), "z\n").unwrap();
            git(&["commit", "-qam", "main moved"]);
        }
        git(&["branch", "local/main", "main"]);
        tip
    }

    fn config_answers(&self) -> [(&'static str, &'static str); 5] {
        // The roster names `builder` whether or not it has a rune file; only a rune file gives it a partition.
        let (partitions, for_labels) = if self.scenario == Scenario::Broken { ("ready-work\t\n", "builder\n") } else { ("", "") };
        let task = "builder";
        [("rune task", task), ("rune lane", ""), ("rune partitions", partitions), ("rune for-labels ready-work", for_labels), ("repo landref widgets", "main\n")]
    }

    fn set_now(&self, secs: u64) {
        fs::write(self.p("now.secs"), format!("{secs}\n")).unwrap();
        fs::write(self.p("now.iso"), format!("{}\n", iso_of(secs))).unwrap();
    }

    fn values(&self, bin: &Path) -> BTreeMap<String, String> {
        let floor = if self.scenario == Scenario::Broken { "100" } else { "0" };
        let mut v: BTreeMap<String, String> = [
            ("COCKPIT_SESSIONS", "brain".to_string()),
            ("COCKPIT_MAIL", String::new()),
            ("SPIRA_METRICS_ENDPOINT", String::new()),
            ("SPIRA_MAIL", self.p("hmail").display().to_string()),
            ("SPIRA_RECONCILER_SHADOW_KINDS", String::new()),
            ("SPIRA_RECONCILER_EFFECT_PASSES", "2".into()),
            ("SPIRA_QUEUE_DIR", self.p("run/queue").display().to_string()),
            ("SPIRA_REPO_MAP", self.p("rows").display().to_string()),
            ("SPIRA_RELEASES", String::new()),
            ("SPIRA_DOLT_DATA", String::new()),
            ("SPIRA_MAX_LIVE_DWARVES", "5".into()),
            ("SPIRA_LANES_MAX_LIVE", "2".into()),
            ("SPIRA_MAX_DWARVES", "6".into()),
            ("SPIRA_DISK_FLOOR_PCT", floor.into()),
            ("SPIRA_UNTAGGED_IMAGES_MAX", "40".into()),
            ("SPIRA_RECONCILER_GRACE_SECS", "0".into()),
            ("SPIRA_MAIN_HEALTH_ACTIVATION_GRACE_SECS", "1800".into()),
            ("SPIRA_MAIN_HEALTH_FENCE_DEADLINE_SECS", "120".into()),
            ("SPIRA_PREFLIGHT_WALL_SECS", "240".into()),
            ("SPIRA_CTRL", self.p("ctrl.json").display().to_string()),
            ("SPIRA_INSTANCE", "prod".into()),
            ("SPIRA_DESIRED_DIR", self.p("desired").display().to_string()),
            ("SPIRA_NO_PROGRESS_BREAKER_CLAIMS", "4".into()),
            ("SPIRA_NO_PROGRESS_BREAKER_WINDOW_MINUTES", "180".into()),
            ("SPIRA_RUNES", "builder".into()),
            ("SPIRA_STRANDED_RUNNER_MIN_AGE", "300".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        v.insert("SPIRA_RUN".into(), self.p("run").display().to_string());
        v
    }
}

fn seams_in(bin: &Path) -> Seams {
    let tool = |n: &str| bin.join(n).display().to_string();
    Seams::resolving(|n| if n == "dolt-beads.service" { n.into() } else { tool(n) })
}

// ── the oracle ────────────────────────────────────────────────────────────────────────────

fn pass_of_oracle(f: &Fixture, bin: &Path, pass: u64) {
    f.set_now(T0 + pass * PASS_SECS);
    // The two list-valued keys keep the fixture's own: the oracle reads neither of them here.
    let pairs: Vec<(String, String)> = f.values(bin).into_iter().filter(|(k, _)| k != "SPIRA_RUNES" && k != "COCKPIT_SESSIONS").collect();
    let declare: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let toml = spira_config::process::fixture_toml(f.dir.path(), &declare);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    // batch-job: the binary under test, run to completion
    let out = Command::new(debug_dir().join("reconciler"))
        .arg("--pass")
        .env_clear()
        .env("SPIRA_TOML", &toml)
        .env("SPIRA_HOME", &home)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();
    assert!(out.status.success(), "reconciler --pass: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

#[derive(Debug, PartialEq)]
struct Seen {
    remedies: Vec<String>,
    mails: Vec<String>,
}

fn lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path).unwrap_or_default().lines().map(str::to_string).collect()
}

/// `N% free` is read from the kernel at two different instants.
fn steady(s: String) -> String {
    let mut out = String::new();
    let mut digits = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        if c == '%' && !digits.is_empty() {
            digits.clear();
            out.push('N');
        } else {
            out.push_str(&std::mem::take(&mut digits));
        }
        out.push(c);
    }
    out.push_str(&digits);
    out
}

/// Text with the fixture's own directory and the stub programs' directory taken out of it.
fn scrub(f: &Fixture, text: &str) -> String {
    let dir = f.dir.path().to_str().unwrap();
    steady(text.replace(&format!("{dir}/bin-h/"), "").replace(dir, "$T"))
}

fn oracle_mails(f: &Fixture, from: usize) -> Vec<String> {
    let mut all: Vec<(usize, String)> = fs::read_dir(f.p("omail")).unwrap().flatten().map(|e| (e.file_name().to_string_lossy().parse::<usize>().unwrap(), fs::read_to_string(e.path()).unwrap().trim_end().to_string())).collect();
    all.sort();
    all.into_iter().skip(from).map(|(_, m)| scrub(f, &m)).collect()
}

/// The concierge mail delivered since the last call, as `Subject\nbody`.
fn handler_mails(f: &Fixture, seen: &mut std::collections::BTreeSet<PathBuf>) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(f.p("hmail/concierge/new"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| seen.insert(e.path()))
        .map(|e| {
            let text = fs::read_to_string(e.path()).unwrap();
            let (head, body) = text.split_once("\n\n").unwrap();
            let subject = head.lines().find_map(|l| l.strip_prefix("Subject: ")).unwrap();
            scrub(f, &format!("{subject}\n{}", body.trim_end()))
        })
        .collect();
    v.sort();
    v
}

// ── the handler ───────────────────────────────────────────────────────────────────────────

fn mail_env(root: &Path) -> mail::env::Env {
    mail::env::Env {
        mail_root: root.join("hmail"),
        kinds_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/mail/kinds"),
        index_file: root.join("mail-index"),
        mute: false,
        loom_budget_ms: 1000,
        repeat_window_s: 600,
        tidy_fresh_s: 0,
        id_prefix: "sp".into(),
        ask_label: "ask".into(),
        db: String::new(),
        bd_bin: String::new(),
        bd_conn_retries: 1,
        operator_actor: "operator".into(),
        run_dir: root.join("run"),
        home: root.to_path_buf(),
        mail_from: None,
        lint_considered: None,
        repeat_considered: None,
        operator_considered: None,
        statute_guards: String::new(),
        statute_considered: None,
        allow_blocking: false,
        bead_id: None,
        lock_timeout_ms: 1000,
    }
}

struct Host {
    engine: Engine,
    mem: Arc<MemJournal>,
    clock: VirtualClock,
    fx: Effects,
}

impl Host {
    fn start(f: &Fixture, bin: &Path) -> Host {
        let broken = f.scenario == Scenario::Broken;
        let (unit, state) = if broken { ("spira-dead-prod.service", None) } else { ("spira-ok-prod.service", Some("active")) };
        let loaded: Vec<(String, String)> = state.map(|s| (unit.to_string(), s.to_string())).into_iter().collect();
        serve_bus(&f.p("bus"), loaded, vec![(format!("/u/{unit}"), "enabled".into())]);
        serve_lc(&f.p("lc.sock"), f.scenario, f.tip.clone());
        let created = iso_of(T0 - 900);
        let api = serve_forge(f.scenario, created);

        let values = f.values(bin);
        let probe = reconciler::Config::from_lookup(&|k| values.get(k).cloned().ok_or_else(|| format!("{k} unknown")), Seams::production(), f.p("run"), String::new(), 0, String::new()).unwrap();
        let rules: Rules = probe.rules();
        let reconciler = Reconciler { interval_ms: PASS_SECS * 1000, rules, values, home: f.p("home").display().to_string() };
        let world = World { run_dir: f.p("run"), queue_dir: f.p("run/queue"), repo_map: f.p("rows"), home_repo: "widgets".into(), lc_socket: f.p("lc.sock"), ..World::default() };
        let fx = Effects {
            world,
            forge: Forge { api, token: String::new() },
            mail: Some(Arc::new(mail_env(f.dir.path()))),
            reconciler: Some(reconciler.clone()),
            seams: Some(seams_in(bin)),
            bus: Some(f.p("bus").display().to_string()),
            ..Effects::default()
        };
        fx.enabled.set(unit.to_string()).unwrap();
        // What spira-config prints for these questions, as fixtures that stand on their own: the
        // oracle's stub is seeded from them, and the handler's in-process reading must agree.
        for (argv, want) in f.config_answers() {
            let host = spira_runtime::reconcile_host::RuntimeHost::new(fx.world.clone(), Forge::default(), None, f.p("home"), "builder".into(), 300, T0, unit.into(), Default::default());
            use reconciler::Host as _;
            let parts: Vec<&str> = argv.split(' ').collect();
            assert_eq!(host.config(&parts), want, "spira-config {argv}: the in-process reading disagrees with the fixture");
        }
        let clock = VirtualClock::new(T0 * 1000);
        let mem = MemJournal::new();
        let mut engine = Engine::open(mem.clone(), Arc::new(clock.clone()), vec![reconciler.registration()], 0).unwrap();
        let (id, stream, kind) = Reconciler::boot();
        engine.submit(id, stream, kind).unwrap();
        Host { engine, mem, clock, fx }
    }

    fn settle(&mut self) {
        loop {
            self.engine.run_until_idle().unwrap();
            let pending = self.engine.pending_effects();
            if pending.is_empty() {
                break;
            }
            for p in pending {
                let out = match self.fx.run(&p.key, &p.spec) {
                    Ok(d) => Outcome::Succeeded(d),
                    Err(d) => Outcome::Failed(d),
                };
                self.engine.complete(&p.key, out).unwrap();
            }
        }
    }

    /// Run one pass: settle everything the tick asked for, then move the clock to the next tick.
    fn pass(&mut self) {
        self.settle();
        self.clock.advance(PASS_SECS * 1000);
    }

    fn journaled(&self) -> Vec<Event> {
        self.mem.read_from(0, 100_000).unwrap().into_iter().map(|(_, e)| e).collect()
    }

    fn book(&self) -> Book {
        let state = self.journaled().into_iter().rev().find(|e| e.kind.tag == handler::STATE);
        state.map(|e| serde_json::from_str(&e.kind.body).unwrap()).unwrap_or_default()
    }
}

/// A book as text, scrubbed.
fn shown(b: &Book, f: &Fixture) -> String {
    scrub(f, &serde_json::to_string(b).unwrap())
}

fn oracle_book(f: &Fixture) -> Book {
    Book {
        state: reconciler_engine::io::load_state(&f.p("run/reconciler-state.json")),
        pending: reconciler_engine::effect::load_pending(&f.p("run/reconciler-effects.json")),
        alerted: reconciler_engine::io::load_alerted(&f.p("run/reconciler-alerted.json")),
        reminded: Default::default(),
    }
}

#[test]
fn e2e_the_handler_and_the_binary_make_the_same_remedies_mails_and_state_over_three_passes() {
    assert!(debug_dir().join("reconciler").exists(), "build reconciler first: it is the oracle");
    let old = Fixture::build("old", Scenario::Broken);
    let new = Fixture::build("new", Scenario::Broken);
    let (obin, hbin) = (old.stubs("o"), new.stubs("h"));
    let mut host = Host::start(&new, &hbin);
    let mut mails_seen = 0;
    let mut delivered = std::collections::BTreeSet::new();
    let mut every_mail = vec![];
    let mut remedies_seen = (0, 0);
    let mut all_remedies = 0;

    for pass in 0..3u64 {
        pass_of_oracle(&old, &obin, pass);
        assert_eq!(host.clock.now_ms(), (T0 + pass * PASS_SECS) * 1000, "the two sides' passes are at the same time");
        host.pass();

        let in_t = |f: &Fixture, side: &str| -> Vec<String> { lines(&f.p(&format!("{side}.remedies"))).into_iter().map(|l| l.replace(f.dir.path().to_str().unwrap(), "$T")).collect() };
        let o_rem = in_t(&old, "o");
        let h_rem = in_t(&new, "h");
        let (o_new, h_new) = (o_rem[remedies_seen.0..].to_vec(), h_rem[remedies_seen.1..].to_vec());
        remedies_seen = (o_rem.len(), h_rem.len());
        let sorted = |mut v: Vec<String>| {
            v.sort();
            v
        };
        let o_mails = oracle_mails(&old, mails_seen);
        mails_seen += o_mails.len();
        let h_mails = handler_mails(&new, &mut delivered);
        every_mail.extend(h_mails.clone());
        let oracle = Seen { remedies: sorted(o_new), mails: sorted(o_mails) };
        let handled = Seen { remedies: sorted(h_new), mails: sorted(h_mails) };
        all_remedies += oracle.remedies.len();
        assert_eq!(handled, oracle, "pass {pass}: the handler and reconciler --pass disagree");
        assert_eq!(shown(&host.book(), &new), shown(&oracle_book(&old), &old), "pass {pass}: hysteresis state, pending remedies and alerted streaks");
    }
    assert!(all_remedies >= 7, "the world must break six things for this to mean anything: {all_remedies} remedy calls");
    let first = lines(&old.p("o.remedies"));
    for want in ["systemctl --user restart spira-dead-prod.service", "target-reap", "spira-lc drop sp-junk", "forge workflow-rerun", "bead dep convert sp-child sp-epic", "queue eject sp-cert"] {
        assert!(first.iter().any(|l| l.contains(want)), "the oracle never ran {want}: {first:?}");
    }
    let mails = every_mail;
    for want in ["RECONCILER: fleet:ready-work", "RECONCILER: junk-row:sp-junk", "RECONCILER: queue-stranded-run:widgets:55", "RECONCILER: epic-blocks:sp-child:sp-epic", "RECONCILER: queue-mergeable:widgets:sp-cert", "RECONCILER: remedy for"] {
        assert!(mails.iter().any(|m| m.starts_with(want)), "no mail began {want}: {mails:?}");
    }
}

#[test]
fn e2e_ten_ticks_with_every_invariant_satisfied_journal_nothing_and_launch_none_of_the_replaced_programs() {
    assert!(debug_dir().join("reconciler").exists(), "build reconciler first: it is the positive control");

    // Positive control: the counter sees the launches the oracle makes in the same world.
    let old = Fixture::build("quiet-old", Scenario::Quiet);
    let obin = old.stubs("o");
    pass_of_oracle(&old, &obin, 0);
    let counted = lines(&old.p("o.calls"));
    for name in COUNTED {
        assert!(counted.iter().any(|l| l.starts_with(&format!("{name} "))), "the oracle never launched {name}, so a count of zero would prove nothing: {counted:?}");
    }
    for question in ["rev-parse --abbrev-ref HEAD", "merge-tree", "worktree add"] {
        assert!(counted.iter().any(|l| l.starts_with("git ") && l.contains(question)), "the oracle never asked git `{question}`: {counted:?}");
    }
    assert!(lines(&old.p("o.remedies")).is_empty() && oracle_mails(&old, 0).is_empty(), "the quiet world is not quiet for the oracle");

    let new = Fixture::build("quiet-new", Scenario::Quiet);
    let hbin = new.stubs("h");
    let mut host = Host::start(&new, &hbin);
    host.pass();
    let after_first = host.journaled().len();
    let git_after_first = lines(&new.p("h.calls")).into_iter().filter(|l| l.starts_with("git ")).collect::<Vec<_>>();
    for _ in 1..10 {
        host.pass();
    }
    let events = host.journaled();
    assert_eq!(events.len(), after_first, "ticks 2 to 10 journaled: {:?}", events[after_first..].iter().map(|e| (e.kind.tag.clone(), e.kind.body.clone())).collect::<Vec<_>>());
    assert!(events.iter().filter(|e| e.kind.tag == handler::STATE).count() <= 1, "more than the first STATE");
    assert_eq!(host.book(), Book::default(), "satisfied checks leave nothing to remember");

    let launched = lines(&new.p("h.calls"));
    for name in COUNTED {
        assert!(!launched.iter().any(|l| l.starts_with(&format!("{name} "))), "the handler launched {name}: {launched:?}");
    }
    let git: Vec<String> = launched.iter().filter(|l| l.starts_with("git ")).cloned().collect();
    assert_eq!(git, git_after_first, "git was asked again after the first tick, though no ref moved");
    assert!(!git.iter().any(|l| l.contains("--abbrev-ref")), "HEAD is read, not asked for: {git:?}");
    assert_eq!(git.iter().filter(|l| l.contains("merge-tree")).count(), 1, "the certified branch's mergeability is asked once: {git:?}");
    assert!(git.iter().any(|l| l.contains("worktree add")) && launched.iter().any(|l| l.starts_with("spira-lint ")), "main-health ran its fences on the first tick: {launched:?}");
    let ticks = events.len();
    assert!(ticks > 0, "the handler never ran: nothing was journaled");
    let stamp = fs::read_to_string(new.p("run/reconciler-pass.stamp")).unwrap_or_default();
    assert!(!stamp.trim().is_empty(), "a pass leaves its stamp for czar-pass to read");
}

fn suspend_due(f: &Fixture) {
    let mut data = spira_ctrl::CtrlData::new();
    spira_ctrl::suspend(&mut data, "spira-other", "waits on tenancy", "sp-owner", "2026-10-07", "operator");
    spira_ctrl::set_until(&mut data, "spira-other", "2026-12-31");
    spira_ctrl::write_atomic(&f.p("ctrl.json"), &data).unwrap();
}

fn mail_requests(host: &Host) -> usize {
    host.journaled().iter().filter(|e| e.kind.tag == EFFECT_REQUESTED && e.kind.body.contains(handler::MAIL)).count()
}

#[test]
fn e2e_a_suspension_that_falls_due_is_reminded_once_and_a_reminder_that_does_not_go_is_sent_again() {
    let f = Fixture::build("remind", Scenario::Quiet);
    suspend_due(&f);
    let mut host = Host::start(&f, &f.stubs("h"));
    let mut seen = Default::default();
    for _ in 0..3 {
        host.pass();
    }
    let sent = handler_mails(&f, &mut seen);
    assert_eq!(sent.len(), 1, "one reminder across three passes: {sent:?}");
    assert!(sent[0].starts_with("reconciler: suspension of spira-other is due for review\n## Note"), "{}", sent[0]);
    assert_eq!(host.book().reminded.len(), 1, "the sent reminder is remembered");

    let g = Fixture::build("remind-down", Scenario::Quiet);
    suspend_due(&g);
    let mut down = Host::start(&g, &g.stubs("h"));
    down.fx.mail = None;
    for _ in 0..3 {
        down.pass();
    }
    assert_eq!(mail_requests(&down), 3, "a reminder that did not go is asked for again every pass");
    assert!(down.book().reminded.is_empty(), "and is not remembered as sent");
}

#[test]
fn e2e_an_escalation_that_does_not_go_is_forgotten_so_the_next_pass_sends_it_again() {
    let f = Fixture::build("alert-down", Scenario::Broken);
    let mut host = Host::start(&f, &f.stubs("h"));
    host.fx.mail = None;
    host.pass();
    assert!(host.book().alerted.is_empty(), "an alert nobody received is not an alert already sent: {:?}", host.book().alerted);
    let first = mail_requests(&host);
    assert!(first >= 1);
    host.pass();
    assert!(mail_requests(&host) > first, "the fleet gap is escalated again");
}

#[test]
fn e2e_a_halted_world_is_stamped_and_left_alone() {
    let f = Fixture::build("halted", Scenario::Broken);
    fs::write(f.p("run/world.halted"), "").unwrap();
    let bin = f.stubs("h");
    let mut host = Host::start(&f, &bin);
    for _ in 0..3 {
        host.pass();
    }
    assert!(lines(&f.p("h.remedies")).is_empty() && handler_mails(&f, &mut Default::default()).is_empty(), "a halted world is not remedied or escalated");
    assert!(!fs::read_to_string(f.p("run/reconciler-pass.stamp")).unwrap_or_default().trim().is_empty(), "the pass is still stamped, so the czar does not call the reconciler dead");
    assert_eq!(host.book(), Book::default());
}
