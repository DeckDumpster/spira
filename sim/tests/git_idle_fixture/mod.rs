#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use sim::Sim;
use spira_runtime::{registrations, tasks};

pub const HOUR: u64 = 3_600_000;
pub const START: u64 = 1_700_001_000_000;
pub const LANDED_S: u64 = START / 1000 - 1_000;

pub struct Fixture {
    pub _dir: testkit::TempDir,
    pub _env: testkit::EnvGuard,
    pub log: PathBuf,
    pub local: PathBuf,
    pub pr: PathBuf,
    pub forge: PathBuf,
    pub wiki: PathBuf,
    pub overrides: PathBuf,
    pub toml: PathBuf,
    pub seen: Arc<Mutex<Vec<(String, String)>>>,
    pub programs: PathBuf,
    pub started: PathBuf,
    pub sock: PathBuf,
    pub requests: Arc<Mutex<Vec<String>>>,
    pub refused: Arc<Mutex<Vec<String>>>,
    pub claimable: Arc<Mutex<String>>,
    pub registry: PathBuf,
    pub images: PathBuf,
    pub ready: Arc<Mutex<Option<String>>>,
    pub effects: Arc<spira_runtime::Effects>,
    pub units: Arc<runtime::units::ScriptedUnits>,
    pub proc_root: PathBuf,
    pub release: PathBuf,
    pub _serial: std::sync::MutexGuard<'static, ()>,
    pub watchers: PathBuf,
    pub worktrees: PathBuf,
    pub published: Arc<Mutex<Vec<String>>>,
    pub checkout_rows: Arc<Mutex<Vec<(String, String)>>>,
}

pub fn real_git() -> PathBuf {
    static REAL: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    REAL.get_or_init(|| std::env::split_paths(&std::env::var_os("PATH").unwrap()).map(|d| d.join("git")).find(|g| g.is_file()).expect("a git on PATH")).clone()
}

pub fn real_tool(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap()).map(|d| d.join(name)).find(|g| g.is_file()).unwrap_or_else(|| panic!("a {name} on PATH"))
}

pub fn git(repo: &Path, args: &[&str], at: u64) {
    let date = format!("@{at} +0000");
    let o = Command::new(real_git()).arg("-C").arg(repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).env("GIT_AUTHOR_DATE", &date).env("GIT_COMMITTER_DATE", &date).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

pub fn commit(repo: &Path, msg: &str, at: u64) {
    git(repo, &["commit", "-q", "--allow-empty", "-m", msg], at);
}

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

pub type EnvEdit = fn(&[(&str, Option<&str>)]) -> testkit::EnvGuard;

pub fn fixture(edit_env: EnvEdit) -> Fixture {
    let serial = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = testkit::TempDir::new("git-idle");
    let real = real_git();
    let log = dir.join("git.log");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    testkit::write_exe(bin.join("git"), &format!("#!/bin/sh\necho \"$PWD :: $*\" >> {}\nexec {} \"$@\"\n", log.display(), real.display()));
    let started = dir.join("started.log");
    let release = dir.join("release");
    let registry = process_registry();
    std::fs::write(&registry, "rows 1").unwrap();
    let _ = std::fs::remove_file(registry.with_extension("sqlite-wal"));
    let graph_root = dir.join("graph");
    let images = graph_root.join("overlay-images/images.json");
    std::fs::create_dir_all(images.parent().unwrap()).unwrap();
    std::fs::write(&images, "[]").unwrap();
    let rows = r#"{"rows":[{"bead":"sp-aaaaaa","test":"t","state":"disabled","until":4102444800}]}"#;
    for d in [release.join("bin"), release.join("spira"), bin.clone()] {
        std::fs::create_dir_all(&d).unwrap();
        for name in LOGGED_PROGRAMS {
            let reply = match name {
                "test-registry" => format!("echo '{rows}'"),
                "podman" => format!("case \"$1\" in info) echo {};; *) echo sha;; esac", graph_root.display()),
                "date" | "timeout" => format!("exec {} \"$@\"", real_tool(name).display()),
                _ => "echo '[]'".to_string(),
            };
            testkit::write_exe(d.join(name), &format!("#!/bin/sh\necho \"{name} $*\" >> {}\n{reply}\n", started.display()));
        }
    }
    for d in [release.join("bin"), bin.clone()] {
        testkit::write_exe(d.join("timeout"), &format!("#!/bin/sh\necho \"timeout $*\" >> {}\nshift\nexec \"$@\"\n", started.display()));
    }
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());

    let local = dir.join("local");
    std::fs::create_dir(&local).unwrap();
    git(&local, &["init", "-q", "-b", "main"], 1);
    std::fs::write(local.join("Cargo.toml"), "[dependencies]\ndumpster-kit = \"1.0\"\n").unwrap();
    std::fs::write(local.join("Cargo.lock"), "[[package]]\nname = \"dumpster-kit\"\nversion = \"1.0.0\"\n").unwrap();
    git(&local, &["add", "."], 1);
    commit(&local, "sp-aaaaaa landed", LANDED_S);
    git(&local, &["branch", "local/main"], 1);
    let forge = dir.join("forge-local.git");
    git(dir.path(), &["init", "-q", "--bare", "-b", "main", forge.to_str().unwrap()], 1);
    git(&local, &["remote", "add", "origin", forge.to_str().unwrap()], 1);
    git(&local, &["push", "-q", "origin", "main"], 1);
    git(&local, &["fetch", "-q", "origin"], 1);

    let wiki = dir.join("wiki");
    std::fs::create_dir(&wiki).unwrap();
    git(&wiki, &["init", "-q", "-b", "main"], 1);
    std::fs::write(wiki.join("page.md"), "page").unwrap();
    git(&wiki, &["add", "."], 1);
    commit(&wiki, "page", LANDED_S);

    let acc = dir.join("acc");
    std::fs::create_dir(&acc).unwrap();
    git(&acc, &["init", "-q", "-b", "main"], 1);
    commit(&acc, "release", LANDED_S);
    git(&acc, &["tag", "spira-1.0.0"], 1);
    git(&acc, &["notes", "--ref=acceptance", "add", "-m", "PASS", "HEAD"], 1);

    let kit = dir.join("kit");
    std::fs::create_dir_all(kit.join("du/mp")).unwrap();
    std::fs::write(kit.join("du/mp/dumpster-kit"), "{\"vers\":\"1.0.0\",\"yanked\":false}\n").unwrap();

    let remote = dir.join("remote.git");
    git(dir.path(), &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()], 1);
    let pr = dir.join("pr");
    std::fs::create_dir(&pr).unwrap();
    git(&pr, &["init", "-q", "-b", "main"], 1);
    commit(&pr, "sp-bbbbbb landed", LANDED_S);
    git(&pr, &["remote", "add", "origin", remote.to_str().unwrap()], 1);
    git(&pr, &["push", "-q", "origin", "main"], 1);
    git(&pr, &["fetch", "-q", "origin"], 1);

    let map = dir.join("repos");
    std::fs::write(&map, format!("spira|{}|queue.local|local/main||\nprrepo|{}|pr|origin/main||\n", local.display(), pr.display())).unwrap();
    let run_dir = process_run();
    if let Err(e) = std::fs::remove_dir_all(&run_dir) {
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "{e}");
    }
    std::fs::create_dir_all(&run_dir).unwrap();
    let sock = process_socket();
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let requests: Arc<Mutex<Vec<String>>> = Arc::default();
    let refused: Arc<Mutex<Vec<String>>> = Arc::default();
    let claimable = Arc::new(Mutex::new(String::from("{\"beads\":[]}")));
    let ready_id: Arc<Mutex<Option<String>>> = Arc::default();
    let (asked, turned_away, ready, bead) = (requests.clone(), refused.clone(), claimable.clone(), ready_id.clone());
    let published: Arc<Mutex<Vec<String>>> = Arc::default();
    let served = published.clone();
    let checkout_rows: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let answering_rows = checkout_rows.clone();
    std::thread::spawn(move || runtime::socket::answer(&listener, &|argv| {
        asked.lock().unwrap().push(argv.join(" "));
        match argv.first().map(String::as_str) {
            Some("rc") => Ok("{\"ok\":{\"list_rev\":1,\"items\":[]}}".into()),
            Some("list") => {
                let ids = argv.iter().position(|a| a == "--ids").and_then(|i| argv.get(i + 1)).map(|v| v.split(',').map(str::to_string).collect::<Vec<_>>());
                let live = argv.iter().any(|a| a == "--live");
                let mut rows: Vec<String> = bead.lock().unwrap().as_deref().map(lc_row).into_iter().collect();
                rows.extend(answering_rows.lock().unwrap().iter().filter(|(id, state)| ids.as_ref().is_none_or(|v| v.contains(id)) && !(live && spira_store::lc_state::is_terminal(state))).map(|(id, state)| lc_row_in(id, state)));
                Ok(format!("[{}]", rows.join(",")))
            }
            Some("plan") if argv.get(1).map(String::as_str) == Some("items") => Ok(format!("{{\"items\":[{}]}}", bead.lock().unwrap().as_deref().map_or(String::new(), plan_row))),
            Some("plan") => Ok("{\"watermark\":1,\"items\":[]}".into()),
            Some("content") => Ok("[]".into()),
            Some("list-asks") => Ok("[]".into()),
            Some("section") if argv.get(1).map(String::as_str) == Some("publish-status") => {
                served.lock().unwrap().push(argv.last().cloned().unwrap_or_default());
                Ok("{\"ok\":{}}".into())
            }
            Some("ops-graph" | "ops-view" | "section" | "audits" | "reconcile-content") => Ok("{}".into()),
            Some("git-fact") => Ok("{}".into()),
            Some("intent") => Ok("{\"ok\":{}}".into()),
            Some("claimable") => Ok(ready.lock().unwrap().clone()),
            _ => {
                turned_away.lock().unwrap().push(argv.join(" "));
                Err(runtime::RuntimeError::new("the double has no such verb"))
            }
        }
    }));
    let home = spira_config::resolve::locate_home_for_process().unwrap();
    let runes_s = home.join("runes").display().to_string();
    let modes = tasks::all().map(|t| format!("{}=act", t.name)).collect::<Vec<_>>().join(" ");
    let run_s = run_dir.display().to_string();
    let watchers = dir.join("watchers");
    std::fs::write(&watchers, "a\n").unwrap();
    let watchers_s = watchers.display().to_string();
    let (wiki_s, prod_s, registry_s) = (wiki.display().to_string(), acc.join("spira").display().to_string(), format!("file://{}", kit.display()));
    let release_s = release.display().to_string();
    let toml = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_RUN", &run_s), ("SPIRA_WATCHERS", &watchers_s), ("SPIRA_HOST_WORKTREE_SUBDIR", "wt"), ("SPIRA_RUNES_DIR", &runes_s), ("SPIRA_WIKI", &wiki_s), ("SPIRA_PROD", &prod_s), ("SPIRA_KIT_REGISTRY", &registry_s), ("SPIRA_KIT_CRATE", "dumpster-kit"), ("SPIRA_KIT_SITES", "spira:Cargo.toml"), ("SPIRA_ID_PREFIX", "sp"), ("SPIRA_BATCHER_FORGE_ENABLE", "1"), ("SPIRA_LC_SOCKET", sock.to_str().unwrap()), ("SPIRA_TASK_MODES", &modes), ("SPIRA_REPO_MAP", map.to_str().unwrap()), ("SPIRA_RUNTIME_OVERRIDES_SECS", "60"), ("SPIRA_RELEASE", &release_s), ("SPIRA_TEST_REGISTRY_PATH", registry.to_str().unwrap()), ("SPIRA_CR_TZ", "UTC"), ("SPIRA_CR_OPEN_HOUR", "9")]);
    let programs = dir.join("programs.log");
    let trap = format!("#!/bin/sh\necho \"spira-lc $*\" >> {}\nexit 1\n", programs.display());
    testkit::write_exe(release.join("bin/spira-lc"), &trap);
    testkit::write_exe(bin.join("spira-lc"), &trap);
    let incident = dir.join("incident.sh");
    testkit::write_exe(&incident, "#!/bin/sh\nexit 0\n");
    let psi = dir.join("psi");
    std::fs::create_dir(&psi).unwrap();
    std::fs::write(psi.join("io"), "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n").unwrap();
    let unit_home = dir.join("home");
    std::fs::create_dir_all(unit_home.join(".config/systemd/user")).unwrap();
    let env = edit_env(&[
        ("HOME", unit_home.to_str()),
        ("PATH", Some(&path)),
        ("SPIRA_LC_BIN", None),
        ("SPIRA_INCIDENT_SH", incident.to_str()),
        ("SPIRA_PSI_DIR", psi.to_str()),
        ("SPIRA_WATCHERS", watchers.to_str()),
        ("SPIRA_RELEASE", release.to_str()),
        ("SPIRA_UNITS", Some("scripted")),
        ("SPIRA_TOML", toml.to_str()),
        ("SPIRA_HOME", home.to_str()),
        ("SPIRA_REPO_MAP", map.to_str()),
        ("SPIRA_HOME_REPO", Some("spira")),
        ("SPIRA_REPO", local.to_str()),
    ]);
    let overrides = dir.join("overrides");
    std::fs::create_dir(&overrides).unwrap();
    let worktrees = run_dir.join("wt");
    std::fs::create_dir_all(&worktrees).unwrap();
    let proc_root = dir.join("proc");
    for (pid, comm, ppid) in [(100, "tmux: server", 1), (101, "bash", 100), (102, "lc-view", 100), (200, "tmux: client", 50)] {
        std::fs::create_dir_all(proc_root.join(pid.to_string())).unwrap();
        std::fs::write(proc_root.join(pid.to_string()).join("stat"), format!("{pid} ({comm}) S {ppid} 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 {} {pid}0 0\n", ["0"; 2].join(" "))).unwrap();
    }
    for role in ["health", "layout"] {
        std::fs::write(release.join("bin").join(role), "").unwrap();
    }
    let units = Arc::new(runtime::units::ScriptedUnits::new(Arc::new(runtime::ports::WallClock)));
    units.install("dolt-beads.service", runtime::units::UnitFacts { state: runtime::units::UnitState::Active, ..runtime::units::UnitFacts::absent() });
    units.install(ENSURE_UNIT, runtime::units::UnitFacts { state: runtime::units::UnitState::Inactive, main_pid: None, exit: None, since_ms: None });
    let world = spira_runtime::world::World {
        overrides_dir: overrides.clone(),
        lc_socket: sock.clone(),
        repo_map: map,
        home_repo: "spira".into(),
        run_dir: run_dir.clone(),
        wiki: wiki.clone(),
        cr_tz: "UTC".into(),
        cr_open_hour: "9".into(),
        proc_dir: proc_root.clone(),
        ..Default::default()
    };
    let effects = Arc::new(spira_runtime::Effects {
        world,
        units: Some(units.clone()),
        ..Default::default()
    });
    Fixture {
        _serial: serial,
        _env: env,
        _dir: dir,
        log,
        local,
        pr,
        forge,
        wiki,
        overrides,
        toml,
        seen: Arc::default(),
        programs,
        started,
        sock: sock.clone(),
        requests,
        refused,
        claimable,
        ready: ready_id,
        registry,
        images,
        effects,
        watchers,
        worktrees,
        published,
        checkout_rows,
        units,
        proc_root,
        release,
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.sock);
    }
}

pub fn process_registry() -> PathBuf {
    static HOME: std::sync::OnceLock<testkit::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| testkit::TempDir::new("git-idle-registry")).join("registry.sqlite")
}

pub fn process_run() -> PathBuf {
    static HOME: std::sync::OnceLock<testkit::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| testkit::TempDir::new("git-idle-run")).join("run")
}

pub fn process_socket() -> PathBuf {
    static HOME: std::sync::OnceLock<testkit::TempDir> = std::sync::OnceLock::new();
    let sock = HOME.get_or_init(|| testkit::TempDir::new("git-idle-sock")).join("lc.sock");
    let _ = std::fs::remove_file(&sock);
    sock
}

pub fn lc_row(id: &str) -> String {
    lc_row_in(id, "READY")
}

pub fn lc_row_in(id: &str, state: &str) -> String {
    format!("{{\"bead_id\":\"{id}\",\"state\":\"{state}\",\"holds\":[],\"stack\":{{}},\"version\":\"1\"}}")
}

pub fn plan_row(id: &str) -> String {
    format!("{{\"id\":\"{id}\",\"title\":\"t\",\"priority\":2,\"issue_type\":\"task\",\"status\":\"open\",\"labels\":[\"spira\",\"plan\"],\"created_at\":\"2020-01-01T00:00:00Z\",\"updated_at\":\"2020-01-01T00:00:00Z\"}}")
}

pub const IDLE_TASK_PROGRAMS: [&str; 5] = ["mail-health.sh", "bash", "bead.sh", "schema.sh", "spira-sendmail"];
pub const DATE_RUNS_PER_OPEN_EPOCH: usize = 4;
pub const ENSURE_UNIT: &str = spira_runtime::tasks::g::cockpit_ensure::UNIT;
pub const LOGGED_PROGRAMS: [&str; 21] = ["tmux", "spira-claim", "systemctl", "mail-health.sh", "bash", "bead.sh", "schema.sh", "spira-sendmail", "spira-config", "target-reap", "watchd", "test-registry", "podman", "date", "timeout", "tsd-write", "round-vm", "gate", "queue", "rebase-stale", "testenv"];

pub fn logged_programs(f: &Fixture) -> Vec<String> {
    let all = std::fs::read_to_string(&f.started).unwrap_or_default();
    all.lines().filter(|l| l.starts_with("spira-claim ") || l.contains(" list-units") || IDLE_TASK_PROGRAMS.iter().any(|p| l.split_whitespace().next() == Some(p))).map(str::to_string).collect()
}

// `date -d @N +%H` is the idea prompt's own hour read, not one of the four reads under test.
pub fn started(f: &Fixture, program: &str) -> usize {
    let prefix = format!("{program} ");
    std::fs::read_to_string(&f.started).unwrap_or_default().lines().filter(|l| !l.ends_with(" +%H")).filter(|l| *l == program || l.starts_with(&prefix)).count()
}

pub fn assert_stubs_log(f: &Fixture) {
    let quiet = |p: &str, args: &[&str]| Command::new(p).args(args).stdout(std::process::Stdio::null()).status().unwrap();
    quiet("spira-claim", &["ready-count", "control"]);
    quiet("systemctl", &["--user", "list-units", "control"]);
    assert_eq!(logged_programs(f).len(), 2, "the logging stubs are what PATH resolves, so a started claim or listing is seen");
    for (name, args) in [("test-registry", ["list", "--json"]), ("podman", ["images", "x"]), ("date", ["-u", "+%s"]), ("timeout", ["5", "date"])] {
        quiet(name, &args);
        let key = if name == "timeout" { "timeout 5 date" } else { name };
        assert_eq!(started(f, key), 1, "the {name} stub is what PATH resolves");
    }
    std::fs::remove_file(&f.started).unwrap();
}

pub fn untagged_images(f: &Fixture) -> reconciler::Config {
    let seams = reconciler::Seams::resolving(|n| Ok(n.to_string())).map(|s| reconciler::Seams { podman: f.podman().display().to_string(), ..s }).unwrap();
    reconciler::Config::from_lookup(&|_| Ok("1".into()), seams, process_run(), f.dir_path().display().to_string(), 0, String::new()).unwrap()
}

pub fn make_ready(f: &Fixture, id: &str) {
    let bead = format!("{{\"bead_id\":\"{id}\",\"verdict\":\"claimable\",\"runes\":[],\"row\":{},\"content\":{}}}", lc_row(id), plan_row(id));
    *f.claimable.lock().unwrap() = format!("{{\"beads\":[{bead}]}}");
    *f.ready.lock().unwrap() = Some(id.into());
}

pub fn strand_rows(f: &Fixture) -> Vec<String> {
    let snap: serde_json::Value = serde_json::from_str(&last_output(f, "sentinel-pass.observe")).unwrap_or_default();
    snap["strand"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(str::to_string)).collect()
}

pub const COCKPIT_UNSENT: &str = r#"{"probes":[{"name":"cockpit.unsent","timeout_ms":300000}],"config":{}}"#;

pub fn doubles(f: &Fixture, names: &[&'static str]) -> Arc<sim::doubles::Doubles> {
    let mut d = sim::doubles::service();
    for name in names.iter().copied() {
        let (e, seen) = (f.effects.clone(), f.seen.clone());
        d = d.double(name, move |input| {
            use runtime::host::EffectRunner;
            let input = if name == "cockpit-tier-600s.observe" { COCKPIT_UNSENT } else { input };
            let out = e.run("k", &runtime::EffectSpec::keyed(name, "k", input.to_string(), 60_000));
            seen.lock().unwrap().push((name.to_string(), out.clone().unwrap_or_else(|e| e)));
            out
        });
    }
    Arc::new(d)
}

pub fn run_hours(f: &Fixture, names: &[&'static str], late: (&[&'static str], u64), until: &[u64], between: &mut dyn FnMut(u64)) {
    let mut sim = Sim::new(1);
    for r in registrations(&spira_runtime::Settings::load().expect("settings")) {
        sim.add_workflow(r);
    }
    let every: Vec<&'static str> = names.iter().chain(late.0).copied().collect();
    sim.set_effects(doubles(f, &every));
    for t in tasks::all().filter(|t| t.effects.iter().any(|e| every.contains(e))) {
        let (_, stream, kind) = (t.boot)();
        let at = if t.effects.iter().any(|e| late.0.contains(e)) { late.1 } else { START };
        sim.boot_workflow(at, stream, &kind.tag, &kind.body);
    }
    for t in until {
        sim.run_until(*t);
        between(*t);
    }
}

pub fn rewrite(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
}

pub fn logged(f: &Fixture) -> Vec<String> {
    std::fs::read_to_string(&f.log).unwrap_or_default().lines().map(str::to_string).collect()
}

pub const MINUTE: u64 = 60_000;

pub fn programs_started(f: &Fixture) -> Vec<String> {
    std::fs::read_to_string(&f.programs).unwrap_or_default().lines().map(str::to_string).collect()
}

pub fn clear_programs(f: &Fixture) {
    std::fs::write(&f.programs, "").unwrap();
}

pub fn assert_traps_and_double_are_live(f: &Fixture) {
    let named = spira_store::lifecycle_row::lc_bin().expect("lc_bin resolves through the fixture release");
    let _ = Command::new(&named).output().unwrap();
    let _ = Command::new("spira-lc").output().unwrap();
    assert_eq!(programs_started(f).len(), 2, "the trap logs a run of the program lc_bin names and one by bare name: {:?}", programs_started(f));
    clear_programs(f);
    let before = f.requests.lock().unwrap().len();
    let got = spira_store::lc_call::LcClient::configured().unwrap().call(&["claimable"]);
    assert_eq!(got.expect("the double answers claimable").out, "{\"beads\":[]}");
    assert_eq!(f.requests.lock().unwrap().len(), before + 1, "the call is one recorded request");
    assert!(programs_started(f).is_empty(), "the call started a program: {:?}", programs_started(f));
    let ready = "{\"beads\":[{\"id\":\"sp-zzzzzz\"}]}";
    *f.claimable.lock().unwrap() = ready.into();
    let set = spira_store::lc_call::LcClient::configured().unwrap().call(&["claimable"]).expect("claimable").out;
    *f.claimable.lock().unwrap() = "{\"beads\":[]}".into();
    assert_eq!(set, ready, "a bead set on the double is what claimable serves");
}

pub fn assert_only_the_double_answered(f: &Fixture) {
    assert!(!f.requests.lock().unwrap().is_empty(), "the double recorded no request, so the run proves nothing about the store reads");
    assert_eq!(*f.refused.lock().unwrap(), Vec::<String>::new(), "the double refused verbs the loop asked");
    assert_eq!(programs_started(f), Vec::<String>::new(), "a store read started spira-lc");
}

pub fn last_output(f: &Fixture, name: &str) -> String {
    f.seen.lock().unwrap().iter().rev().find(|(n, _)| n == name).map(|(_, o)| o.clone()).unwrap_or_default()
}


impl Fixture {
    pub fn podman(&self) -> PathBuf {
        self.dir_path().join("bin/podman")
    }

    pub fn dir_path(&self) -> &Path {
        self._dir.path()
    }
}

pub fn touch(path: &Path, secs_after_epoch: u64) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs_after_epoch)).unwrap();
}
pub const WATCHTOWER_PROGRAMS: [&str; 5] = ["systemctl", "spira-config", "target-reap", "timeout", "watchd"];

pub fn started_by_the_watchtower_checks(f: &Fixture) -> Vec<String> {
    let all = std::fs::read_to_string(&f.started).unwrap_or_default();
    all.lines().filter(|l| WATCHTOWER_PROGRAMS.iter().any(|p| l.starts_with(&format!("{p} "))) && !l.starts_with("spira-config resolve ") && !l.starts_with("spira-config env-bootstrap ") && !(l.starts_with("timeout ") && !l.contains(" watchd"))).map(str::to_string).collect()
}

pub fn last_host_section(f: &Fixture) -> serde_json::Value {
    let body = f.published.lock().unwrap().iter().rev().find(|b| b.contains("certify_par")).cloned().unwrap_or_default();
    serde_json::from_str(&body).unwrap_or_default()
}

pub fn host_level(f: &Fixture, field: &str) -> f64 {
    last_host_section(f)["status"][field]["value"].as_f64().unwrap_or(-1.0)
}

pub fn stale_target(f: &Fixture, id: &str) {
    let wt = f.worktrees.join(id);
    std::fs::create_dir_all(wt.join("target")).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
    for d in [wt.join("target"), wt.clone()] {
        std::fs::File::open(&d).unwrap().set_modified(old).unwrap();
    }
}


pub fn argv_counts(lines: &[String]) -> String {
    let mut by: std::collections::BTreeMap<String, usize> = Default::default();
    for l in lines {
        let argv = l.split_once(" :: ").map_or(l.as_str(), |(_, a)| a);
        let verb = argv.split_whitespace().skip_while(|w| w.starts_with('-') || w.starts_with('/') || *w == "-C" || w.contains('=')).take(2).collect::<Vec<_>>().join(" ");
        *by.entry(verb).or_default() += 1;
    }
    by.iter().map(|(k, v)| format!("{v} {k}")).collect::<Vec<_>>().join(", ")
}
