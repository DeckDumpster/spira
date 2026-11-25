//! With no ref moved, the scheduler's per-minute tasks start no git; a moved ref wakes only the
//! reads that depend on it. This binary sets PATH, so it is its own process.

#[macro_use]
extern crate spira_test_id;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use sim::Sim;
use spira_runtime::{registrations, tasks};

#[path = "it/common/mod.rs"]
mod common;

const HOUR: u64 = 3_600_000;
const START: u64 = 1_700_001_000_000;
const LANDED_S: u64 = START / 1000 - 1_000;

struct Fixture {
    _dir: testkit::TempDir,
    _env: testkit::EnvGuard,
    stray: Option<PathBuf>,
    log: PathBuf,
    local: PathBuf,
    pr: PathBuf,
    forge: PathBuf,
    wiki: PathBuf,
    overrides: PathBuf,
    toml: PathBuf,
    seen: Arc<Mutex<Vec<(String, String)>>>,
    programs: PathBuf,
    started: PathBuf,
    sock: PathBuf,
    requests: Arc<Mutex<Vec<String>>>,
    refused: Arc<Mutex<Vec<String>>>,
    claimable: Arc<Mutex<String>>,
    registry: PathBuf,
    images: PathBuf,
    ready: Arc<Mutex<Option<String>>>,
    effects: Arc<spira_runtime::Effects>,
    _serial: std::sync::MutexGuard<'static, ()>,
    watchers: PathBuf,
    worktrees: PathBuf,
    published: Arc<Mutex<Vec<String>>>,
    checkout_rows: Arc<Mutex<Vec<(String, String)>>>,
}

fn real_git() -> PathBuf {
    static REAL: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    REAL.get_or_init(|| std::env::split_paths(&std::env::var_os("PATH").unwrap()).map(|d| d.join("git")).find(|g| g.is_file()).expect("a git on PATH")).clone()
}

fn real_tool(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap()).map(|d| d.join(name)).find(|g| g.is_file()).unwrap_or_else(|| panic!("a {name} on PATH"))
}

fn git(repo: &Path, args: &[&str], at: u64) {
    let date = format!("@{at} +0000");
    let o = Command::new(real_git()).arg("-C").arg(repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).env("GIT_AUTHOR_DATE", &date).env("GIT_COMMITTER_DATE", &date).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

fn commit(repo: &Path, msg: &str, at: u64) {
    git(repo, &["commit", "-q", "--allow-empty", "-m", msg], at);
}

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn fixture() -> Fixture {
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
    let run_dir = dir.join("run");
    std::fs::create_dir(&run_dir).unwrap();
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
    let env = testkit::env(&[
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
    let pinned_run = PathBuf::from(spira_config::process::cfg("SPIRA_RUN").unwrap());
    let worktrees = pinned_run.join("wt");
    std::fs::create_dir_all(&worktrees).unwrap();
    let stray = (!pinned_run.starts_with(dir.path())).then(|| pinned_run.ancestors().find(|a| a.parent() == dir.path().parent()).unwrap().to_path_buf());
    let world = spira_runtime::world::World { overrides_dir: overrides.clone(), lc_socket: sock.clone(), repo_map: map, home_repo: "spira".into(), run_dir, wiki: wiki.clone(), cr_tz: "UTC".into(), cr_open_hour: "9".into(), ..Default::default() };
    let effects = Arc::new(spira_runtime::Effects { world, units: Some(Arc::new(runtime::units::ScriptedUnits::new(Arc::new(runtime::ports::WallClock)))), ..Default::default() });
    Fixture { _serial: serial, _env: env, _dir: dir, stray, log, local, pr, forge, wiki, overrides, toml, seen: Arc::default(), programs, started, sock: sock.clone(), requests, refused, claimable, ready: ready_id, registry, images, effects, watchers, worktrees, published, checkout_rows }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.sock);
        if let Some(p) = &self.stray {
            let _ = std::fs::remove_dir_all(p);
        }
    }
}

fn process_registry() -> PathBuf {
    static HOME: std::sync::OnceLock<testkit::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| testkit::TempDir::new("git-idle-registry")).join("registry.sqlite")
}

fn process_socket() -> PathBuf {
    static HOME: std::sync::OnceLock<testkit::TempDir> = std::sync::OnceLock::new();
    let sock = HOME.get_or_init(|| testkit::TempDir::new("git-idle-sock")).join("lc.sock");
    let _ = std::fs::remove_file(&sock);
    sock
}

fn lc_row(id: &str) -> String {
    lc_row_in(id, "READY")
}

fn lc_row_in(id: &str, state: &str) -> String {
    format!("{{\"bead_id\":\"{id}\",\"state\":\"{state}\",\"holds\":[],\"stack\":{{}},\"version\":\"1\"}}")
}

fn plan_row(id: &str) -> String {
    format!("{{\"id\":\"{id}\",\"title\":\"t\",\"priority\":2,\"issue_type\":\"task\",\"status\":\"open\",\"labels\":[\"spira\",\"plan\"],\"created_at\":\"2020-01-01T00:00:00Z\",\"updated_at\":\"2020-01-01T00:00:00Z\"}}")
}

const IDLE_TASK_PROGRAMS: [&str; 5] = ["mail-health.sh", "bash", "bead.sh", "schema.sh", "spira-sendmail"];
const DATE_RUNS_PER_OPEN_EPOCH: usize = 4;
const LOGGED_PROGRAMS: [&str; 20] = ["spira-claim", "systemctl", "mail-health.sh", "bash", "bead.sh", "schema.sh", "spira-sendmail", "spira-config", "target-reap", "watchd", "test-registry", "podman", "date", "timeout", "tsd-write", "round-vm", "gate", "queue", "rebase-stale", "testenv"];

fn logged_programs(f: &Fixture) -> Vec<String> {
    let all = std::fs::read_to_string(&f.started).unwrap_or_default();
    all.lines().filter(|l| l.starts_with("spira-claim ") || l.contains(" list-units") || IDLE_TASK_PROGRAMS.iter().any(|p| l.split_whitespace().next() == Some(p))).map(str::to_string).collect()
}

// `date -d @N +%H` is the idea prompt's own hour read, not one of the four reads under test.
fn started(f: &Fixture, program: &str) -> usize {
    let prefix = format!("{program} ");
    std::fs::read_to_string(&f.started).unwrap_or_default().lines().filter(|l| !l.ends_with(" +%H")).filter(|l| *l == program || l.starts_with(&prefix)).count()
}

fn assert_stubs_log(f: &Fixture) {
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

fn untagged_images(f: &Fixture) -> reconciler::Config {
    let seams = reconciler::Seams::resolving(|n| Ok(n.to_string())).map(|s| reconciler::Seams { podman: f.podman().display().to_string(), ..s }).unwrap();
    reconciler::Config::from_lookup(&|_| Ok("1".into()), seams, f.dir_path().join("run"), f.dir_path().display().to_string(), 0, String::new()).unwrap()
}

fn make_ready(f: &Fixture, id: &str) {
    let bead = format!("{{\"bead_id\":\"{id}\",\"verdict\":\"claimable\",\"runes\":[],\"row\":{},\"content\":{}}}", lc_row(id), plan_row(id));
    *f.claimable.lock().unwrap() = format!("{{\"beads\":[{bead}]}}");
    *f.ready.lock().unwrap() = Some(id.into());
}

fn strand_rows(f: &Fixture) -> Vec<String> {
    let snap: serde_json::Value = serde_json::from_str(&last_output(f, "sentinel-pass.observe")).unwrap_or_default();
    snap["strand"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(str::to_string)).collect()
}

const COCKPIT_UNSENT: &str = r#"{"probes":[{"name":"cockpit.unsent","timeout_ms":300000}],"config":{}}"#;

fn doubles(f: &Fixture, names: &[&'static str]) -> Arc<sim::doubles::Doubles> {
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

fn run_hours(f: &Fixture, names: &[&'static str], late: (&[&'static str], u64), until: &[u64], between: &mut dyn FnMut(u64)) {
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

fn rewrite(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
}

fn logged(f: &Fixture) -> Vec<String> {
    std::fs::read_to_string(&f.log).unwrap_or_default().lines().map(str::to_string).collect()
}

const MINUTE: u64 = 60_000;

fn programs_started(f: &Fixture) -> Vec<String> {
    std::fs::read_to_string(&f.programs).unwrap_or_default().lines().map(str::to_string).collect()
}

fn clear_programs(f: &Fixture) {
    std::fs::write(&f.programs, "").unwrap();
}

fn assert_traps_and_double_are_live(f: &Fixture) {
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

fn assert_only_the_double_answered(f: &Fixture) {
    assert!(!f.requests.lock().unwrap().is_empty(), "the double recorded no request, so the run proves nothing about the store reads");
    assert_eq!(*f.refused.lock().unwrap(), Vec::<String>::new(), "the double refused verbs the loop asked");
    assert_eq!(programs_started(f), Vec::<String>::new(), "a store read started spira-lc");
}

#[test_id(tqoiwy1)]
#[test]
fn an_idle_hour_after_warm_up_logs_zero_git_and_a_moved_ref_wakes_only_its_own_reads() {
    let f = fixture();
    spira_config::process::with_fresh(|| {
    assert_traps_and_double_are_live(&f);
    assert_stubs_log(&f);
    let warm = START + 10 * MINUTE;
    let (mut at_warm, mut at_idle, mut at_commit, mut at_branch, mut at_quiet_a, mut at_quiet_b) = (0, 0, 0, 0, 0, 0);
    let (mut programs_at_warm, mut programs_at_idle, mut programs_at_ready, mut idle_passes, mut rows_before_ready) = (0, Vec::new(), Vec::new(), 0, Vec::new());
    let (mut tower_at_warm, mut tower_at_idle, mut tower_at_edit, mut tower_at_boundary, mut level_before) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), (0.0, 0.0));
    let spec = f.overrides.join("round-vm-spill-ec2-always.override");
    std::fs::write(&spec, "needed() { return 1; }\n").unwrap();
    let ran = f.dir_path().join("overrides.log");
    let fail = f.dir_path().join("overrides.fail");
    for bin in [f.dir_path().join("bin"), f.dir_path().join("release/bin")] {
        testkit::write_exe(bin.join("overrides.sh"), &format!("#!/bin/sh\necho \"$*\" >> {}\ntouch {}\n[ -e {} ] && {{ echo 'overrides: ghost: failed' >&2; exit 1; }}\nexit 0\n", ran.display(), f.toml.display(), fail.display()));
    }
    let runs = || std::fs::read_to_string(&ran).unwrap_or_default().lines().count();
    let mut applied: std::collections::BTreeMap<&str, usize> = Default::default();
    let names = ["watch.throughput.observe", "landing.observe", "overrides.apply", "inbox.notify.observe", "asks.ideas.observe", "batcher.local.observe", "cockpit-tier-60s.observe", "cr-cadence.observe"];
    let sentinel = ["sentinel-pass.observe"];
    let reads = ["test-registry", "podman", "date", "timeout 5 date"];
    let counts = |f: &Fixture| reads.map(|p| started(f, p));
    let (mut reads_at_warm, mut reads_at_idle, mut reads_at_registry, mut reads_at_images, mut reads_at_expiry, mut reads_at_midnight) = ([0; 4], [0; 4], [0; 4], [0; 4], [0; 4], [0; 4]);
    let reconciled = untagged_images(&f);
    let (before_hour, after) = (START + 59 * MINUTE, warm + HOUR);
    let m = |n: u64| after + n * MINUTE;
    let (quiet_a, quiet_b) = (after - 7 * MINUTE, after - 4 * MINUTE);
    let mut marks: Vec<u64> = (0..=60).map(|k| warm + k * MINUTE).chain([before_hour, quiet_a, quiet_b, m(3), m(6), m(8), m(9), m(10), m(12), m(15), m(16), m(18), m(21)]).collect();
    marks.sort();
    marks.dedup();
    run_hours(&f, &names, (&sentinel, warm + HOUR - 10 * MINUTE), &marks, &mut |t| {
        let _ = reconciler::observe_untagged_images(&reconciled);
        for days_after in [0, 1] {
            spira_runtime::world::cr_open_epoch("UTC", 9, t / 1000, days_after).unwrap();
        }
        match t {
        t if t == warm => {
            applied.insert("warm", runs());
            (at_warm, programs_at_warm) = (logged(&f).len(), logged_programs(&f).len());
            reads_at_warm = counts(&f);
        }
        t if t == before_hour => drop(applied.insert("idle", runs())),
        t if t == quiet_a => {
            tower_at_warm = started_by_the_watchtower_checks(&f);
            at_quiet_a = logged(&f).len();
        }
        t if t == quiet_b => {
            at_quiet_b = logged(&f).len();
            tower_at_idle = started_by_the_watchtower_checks(&f);
            level_before = (host_level(&f, "certify_par"), host_level(&f, "stale_targets"));
            std::fs::write(&f.watchers, "a\nb\n").unwrap();
        }
        t if t == m(6) => {
            applied.insert("spec", runs());
            touch(&f.toml, 1_000_000_002);
            at_branch = logged(&f).len();
        }
        t if t == m(8) => rewrite(&f.images, "[1]"),
        t if t == m(9) => {
            applied.insert("toml", runs());
            std::fs::write(&fail, "").unwrap();
            touch(&spec, 1_000_000_003);
        }
        t if t == m(10) => reads_at_images = counts(&f),
        t if t == m(12) => {
            applied.insert("failing", runs());
            std::fs::remove_file(&fail).unwrap();
        }
        t if t == m(15) => drop(applied.insert("recovered", runs())),
        t if t == m(18) => {
            applied.insert("recovered-quiet", runs());
            tower_at_edit = started_by_the_watchtower_checks(&f);
            let doc = spira_config::load(&f.toml).unwrap();
            spira_config::serialize_and_write(&f.toml, &spira_config::set_path(&doc, "spira.certify_par", "7").unwrap()).unwrap();
            stale_target(&f, "sp-aaaaaa");
        }
        t if t == m(16) => reads_at_expiry = counts(&f),
        t if t == m(21) => reads_at_midnight = counts(&f),
        t if t == warm + HOUR => {
            applied.insert("boundary", runs());
            tower_at_boundary = started_by_the_watchtower_checks(&f);
            at_idle = logged(&f).len();
            programs_at_idle = logged_programs(&f);
            reads_at_idle = counts(&f);
            idle_passes = f.seen.lock().unwrap().iter().filter(|(n, _)| n == "sentinel-pass.observe").count();
            rows_before_ready = strand_rows(&f);
            make_ready(&f, "sp-zzzzzz");
            commit(&f.local, "sp-cccccc landed", LANDED_S + 10);
            git(&f.local, &["branch", "-f", "local/main"], 1);
            rewrite(&f.registry, "rows 22");
        }
        t if t == m(3) => {
            applied.insert("ref", runs());
            touch(&spec, 1_000_000_001);
            at_commit = logged(&f).len();
            programs_at_ready = logged_programs(&f);
            tower_at_edit = started_by_the_watchtower_checks(&f);
            let doc = spira_config::load(&f.toml).unwrap();
            spira_config::serialize_and_write(&f.toml, &spira_config::set_path(&doc, "spira.certify_par", "7").unwrap()).unwrap();
            stale_target(&f, "sp-aaaaaa");
            reads_at_registry = counts(&f);
            git(&f.pr, &["branch", "spira/sp-x"], 1);
        }
        _ => {}
        }
    });
    assert_only_the_double_answered(&f);
    let c = |k: &str| applied[k];
    assert!(c("warm") >= 1, "the override was never applied: {applied:?}");
    assert_eq!(c("idle"), c("warm"), "idle minutes started overrides.sh: {applied:?}");
    assert_eq!(c("boundary"), c("idle") + 1, "the hour boundary runs it once: {applied:?}");
    assert_eq!(c("ref"), c("boundary") + 1, "a moved landing ref is applied once: {applied:?}");
    assert_eq!(c("spec"), c("ref") + 1, "a touched spec is applied once: {applied:?}");
    assert_eq!(c("toml"), c("spec") + 1, "a touched config file is applied once: {applied:?}");
    assert_eq!(c("failing"), c("toml") + 3, "a failing pass runs again on every tick: {applied:?}");
    assert!(c("recovered") > c("failing"), "{applied:?}");
    assert_eq!(c("recovered-quiet"), c("recovered"), "a success is held again: {applied:?}");
    let all = logged(&f);
    let status_of_local = |l: &String| l.ends_with(&format!("-C {} status --porcelain", f.local.display()));
    let first_read = &all[at_warm..at_idle];
    assert!(first_read.len() == 1 && status_of_local(&first_read[0]), "an idle virtual hour's only git is the drift check's first status read:\n{}", first_read.join("\n"));
    assert_eq!(at_quiet_a, at_quiet_b, "a quiet stretch logged git:\n{}", all[at_quiet_a..at_quiet_b].join("\n"));
    assert!(!f.worktrees.parent().unwrap().join("conditions/drift-unreadable-units.count").exists(), "the units half was blind in the fixture");
    assert!(idle_passes >= 5, "the sentinel pass observed {idle_passes} times by the end of the idle hour");
    assert_eq!(programs_at_idle.len(), programs_at_warm, "an idle hour started programs:\n{}", programs_at_idle[programs_at_warm..].join("\n"));
    let ran = |name: &str| f.seen.lock().unwrap().iter().filter(|(n, _)| n == name).count();
    assert!(ran("inbox.notify.observe") >= 12 && ran("asks.ideas.observe") >= 12, "both tasks ran over the hour: {} {}", ran("inbox.notify.observe"), ran("asks.ideas.observe"));
    assert!(reads_at_warm.iter().take(3).all(|n| *n > 0), "warm-up read the registry, podman and date, so the idle hour proves something: {reads_at_warm:?}");
    assert_eq!(reads_at_idle, reads_at_warm, "an idle hour started test-registry, podman, date or timeout");
    assert_eq!(reads_at_registry[0], reads_at_idle[0] + 2, "a rewritten registry file is read once by each of its two readers");
    assert_eq!(reads_at_registry[1..], reads_at_idle[1..], "and by nothing else");
    assert_eq!(reads_at_midnight[0], reads_at_registry[0], "and not again");
    assert_eq!(reads_at_images[1], reads_at_registry[1] + 1, "a rewritten images.json is listed once");
    assert_eq!(reads_at_images[0], reads_at_registry[0], "and does not touch the registry");
    assert_eq!(reads_at_expiry, reads_at_images, "a quiet stretch after the rewrites starts no read");
    assert_eq!(reads_at_midnight[2] - reads_at_expiry[2], 2 * DATE_RUNS_PER_OPEN_EPOCH, "a crossed local midnight asks date again, once for each of today's and tomorrow's open");
    assert_eq!(reads_at_midnight[1], reads_at_expiry[1]);
    assert_eq!(reads_at_midnight[3], reads_at_expiry[3]);
    assert!(!rows_before_ready.iter().any(|r| r.contains("sp-zzzzzz")), "nothing is ready yet: {rows_before_ready:?}");
    let rows = strand_rows(&f);
    assert!(rows.iter().any(|r| r.contains("sp-zzzzzz")), "a bead made READY shows in the next strand rows: {rows:?}");
    assert_eq!(programs_at_ready.len(), programs_at_warm, "and reading it started no program: {programs_at_ready:?}");
    assert!(!tower_at_warm.is_empty(), "warm-up started none of {WATCHTOWER_PROGRAMS:?}, so the idle hour proves nothing");
    assert_eq!(tower_at_idle, tower_at_warm, "idle sentinel passes started programs:\n{}", tower_at_idle[tower_at_warm.len().min(tower_at_idle.len())..].join("\n"));
    let after_touch: Vec<&String> = tower_at_boundary[tower_at_idle.len()..].iter().collect();
    assert_eq!(after_touch.iter().filter(|l| l.starts_with("watchd units")).count(), 1, "a touched manifest runs watchd units once: {after_touch:?}");
    assert_eq!(after_touch.len(), 1, "and nothing else starts: {after_touch:?}");
    let after_edit: Vec<String> = started_by_the_watchtower_checks(&f)[tower_at_edit.len()..].to_vec();
    assert!(after_edit.iter().all(|l| l.starts_with("watchd units")) && after_edit.len() <= 2, "a changed certify_par and a stale target start no read of theirs; the config file is a manifest input, so only watchd may rerun: {after_edit:?}");
    assert_eq!((host_level(&f, "certify_par"), host_level(&f, "stale_targets")), (7.0, level_before.1 + 1.0), "the next Host section shows both: {}", last_host_section(&f));
    let local = f.local.display().to_string();
    let pr = f.pr.display().to_string();
    let after_commit: Vec<&String> = all[at_idle..at_commit].iter().filter(|l| !l.ends_with("rev-parse --verify --quiet refs/heads/spira/sp-zzzzzz")).collect();
    let logs = |lines: &[&String], repo: &str| lines.iter().filter(|l| l.contains(&format!("-C {repo} log "))).count();
    assert_eq!(logs(&after_commit, &local), 1, "a moved landing ref is read once: {after_commit:?}");
    assert_eq!(after_commit.iter().filter(|l| status_of_local(l)).count(), 1, "the changed checkout is read once: {after_commit:?}");
    assert_eq!(after_commit.len(), 2, "and nothing else asks git: {after_commit:?}");
    let counted: u64 = f.effects.world.landed_on_refs((warm + HOUR + 3 * MINUTE) / 1000, "sp").unwrap().iter().find(|r| r.repo == "spira").map(|r| r.hourly.iter().sum()).unwrap();
    assert_eq!(counted, 2, "landed_on_refs counts the new landing");
    let after_branch = &all[at_commit..at_branch];
    assert!(after_branch.iter().any(|l| l.contains(&format!("-C {pr} ")) && l.contains(" fetch ")), "a spira/* branch makes landing fetch: {after_branch:?}");
    })
    .unwrap();
}

const SUB_HOUR: [&str; 7] = ["publish-backlog.observe", "publish.observe", "cockpit-tier-600s.observe", "trigger.mimir.observe", "intake.gh.observe", "intake.kit_bump.observe", "brain-dirty.observe"];

fn last_output(f: &Fixture, name: &str) -> String {
    f.seen.lock().unwrap().iter().rev().find(|(n, _)| n == name).map(|(_, o)| o.clone()).unwrap_or_default()
}

fn unsent_kv(f: &Fixture) -> String {
    let out: serde_json::Value = serde_json::from_str(&last_output(f, "cockpit-tier-600s.observe")).unwrap_or_default();
    out["fragments"]["cockpit.unsent"]["kv"].to_string()
}

fn argv_counts(lines: &[String]) -> String {
    let mut by: std::collections::BTreeMap<String, usize> = Default::default();
    for l in lines {
        let argv = l.split_once(" :: ").map_or(l.as_str(), |(_, a)| a);
        let verb = argv.split_whitespace().skip_while(|w| w.starts_with('-') || w.starts_with('/') || *w == "-C" || w.contains('=')).take(2).collect::<Vec<_>>().join(" ");
        *by.entry(verb).or_default() += 1;
    }
    by.iter().map(|(k, v)| format!("{v} {k}")).collect::<Vec<_>>().join(", ")
}

#[test_id(t67ro7u)]
#[test]
fn the_reaper_observe_asks_git_only_about_a_checkout_the_cheap_rules_leave_undecided() {
    let f = fixture();
    spira_config::process::with_fresh(|| {
    let names = ["hel.reap_terminal.observe"];
    let root = f.effects.world.run_dir.join("worktree");
    std::fs::create_dir_all(&root).unwrap();
    for n in [".testbatch-1", "sp-live01", "sp-keep01", "sp-term01"] {
        git(&f.local, &["worktree", "add", "-q", "--detach", root.join(n).to_str().unwrap()], 1);
    }
    std::fs::write(root.join("sp-keep01/.spira-keep"), "").unwrap();
    *f.checkout_rows.lock().unwrap() = vec![("sp-live01".into(), "WORKING".into()), ("sp-term01".into(), "WORKING".into())];
    let (warm, idle, after) = (START + HOUR, START + 2 * HOUR, START + 3 * HOUR);
    let (mut at_warm, mut at_idle) = (0, 0);
    let under = root.display().to_string();
    run_hours(&f, &names, (&[], 0), &[warm, idle, after], &mut |t| match t {
        t if t == warm => at_warm = logged(&f).len(),
        t if t == idle => {
            at_idle = logged(&f).len();
            *f.checkout_rows.lock().unwrap() = vec![("sp-live01".into(), "WORKING".into()), ("sp-term01".into(), "LANDED".into())];
        }
        _ => {}
    });
    let all = logged(&f);
    assert!(f.seen.lock().unwrap().iter().filter(|(n, _)| n == names[0]).count() >= 2, "fewer than two passes ran");
    let idle_hour = &all[at_warm..at_idle];
    assert!(!idle_hour.iter().any(|l| l.contains(&under)), "git asked about a kept checkout: {}\n{}", argv_counts(idle_hour), idle_hour.join("\n"));
    let asked: Vec<&String> = all[at_idle..].iter().filter(|l| l.contains(&under)).collect();
    assert!(!asked.is_empty(), "the terminal checkout was never asked about: {}", argv_counts(&all[at_idle..]));
    assert!(asked.iter().all(|l| l.contains("sp-term01")), "git asked about more than the terminal checkout: {}\n{}", argv_counts(&all[at_idle..]), asked.iter().map(|l| l.as_str()).collect::<Vec<_>>().join("\n"));
    for verb in ["rev-parse --git-common-dir", "rev-list -n1 HEAD"] {
        assert!(asked.iter().any(|l| l.contains(verb)), "no {verb:?} on the terminal checkout: {}", argv_counts(&all[at_idle..]));
    }
    })
    .unwrap();
}

#[test_id(trka3uo)]
#[test]
fn the_sub_hour_observes_run_no_git_over_an_idle_hour_and_a_moved_ref_wakes_only_what_reads_it() {
    let f = fixture();
    spira_config::process::with_fresh(|| {
    assert_traps_and_double_are_live(&f);
    let (warm, idle) = (START + HOUR, START + 2 * HOUR);
    let marks = [warm, idle, idle + 16 * MINUTE, idle + 32 * MINUTE, idle + 43 * MINUTE, idle + 54 * MINUTE];
    let (mut at_warm, mut at_idle) = (0, 0);
    let mut unsent_before = String::new();
    let second = f.dir_path().join("second");
    run_hours(&f, &SUB_HOUR, (&[], 0), &marks, &mut |t| match t {
        t if t == warm => at_warm = logged(&f).len(),
        t if t == idle => {
            at_idle = logged(&f).len();
            commit(&f.local, "sp-cccccc landed", LANDED_S + 10);
            git(&f.local, &["branch", "-f", "local/main"], 1);
        }
        t if t == idle + 16 * MINUTE => {
            let backlog = last_output(&f, "publish-backlog.observe");
            assert!(backlog.contains("\"count\":1"), "a landing commit is one unpublished: {backlog}");
            git(f.dir_path(), &["clone", "-q", f.forge.to_str().unwrap(), second.to_str().unwrap()], 1);
            commit(&second, "someone else", LANDED_S + 20);
            git(&second, &["push", "-q", "origin", "HEAD:main"], 1);
            commit(&f.local, "sp-dddddd landed", LANDED_S + 30);
            git(&f.local, &["branch", "-f", "local/main"], 1);
        }
        t if t == idle + 32 * MINUTE => {
            let publish = last_output(&f, "publish.observe");
            assert!(publish.contains("\"diverged\":true"), "a forge moved by someone else is diverged once the landing ref moves: {publish}");
            std::fs::write(f.wiki.join("new.md"), "new").unwrap();
        }
        t if t == idle + 43 * MINUTE => {
            assert!(last_output(&f, "brain-dirty.observe").contains("new.md"), "{}", last_output(&f, "brain-dirty.observe"));
            unsent_before = unsent_kv(&f);
            git(&f.local, &["branch", "spira/sp-x"], 1);
        }
        _ => {}
    });
    assert_only_the_double_answered(&f);
    let all = logged(&f);
    for read in [" show local/main:Cargo.toml", "log --format=%ct %s --after=@"] {
        assert!(all[..at_warm].iter().any(|l| l.contains(read)), "warm-up never ran {read:?}, so the idle hour proves nothing about it");
    }
    assert_eq!(at_idle, at_warm, "an idle virtual hour logged git: {}\n{}", argv_counts(&all[at_warm..at_idle]), all[at_warm..at_idle].join("\n"));
    assert_ne!(unsent_kv(&f), unsent_before, "a new spira/* branch changes the unsent section");
    assert!(!unsent_before.is_empty());
    })
    .unwrap();
}

impl Fixture {
    fn podman(&self) -> PathBuf {
        self.dir_path().join("bin/podman")
    }

    fn dir_path(&self) -> &Path {
        self._dir.path()
    }
}

fn touch(path: &Path, secs_after_epoch: u64) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs_after_epoch)).unwrap();
}
const WATCHTOWER_PROGRAMS: [&str; 5] = ["systemctl", "spira-config", "target-reap", "timeout", "watchd"];

fn started_by_the_watchtower_checks(f: &Fixture) -> Vec<String> {
    let all = std::fs::read_to_string(&f.started).unwrap_or_default();
    all.lines().filter(|l| WATCHTOWER_PROGRAMS.iter().any(|p| l.starts_with(&format!("{p} "))) && !l.starts_with("spira-config resolve ") && !l.starts_with("spira-config env-bootstrap ") && !(l.starts_with("timeout ") && !l.contains(" watchd"))).map(str::to_string).collect()
}

fn last_host_section(f: &Fixture) -> serde_json::Value {
    let body = f.published.lock().unwrap().iter().rev().find(|b| b.contains("certify_par")).cloned().unwrap_or_default();
    serde_json::from_str(&body).unwrap_or_default()
}

fn host_level(f: &Fixture, field: &str) -> f64 {
    last_host_section(f)["status"][field]["value"].as_f64().unwrap_or(-1.0)
}

fn stale_target(f: &Fixture, id: &str) {
    let wt = f.worktrees.join(id);
    std::fs::create_dir_all(wt.join("target")).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
    for d in [wt.join("target"), wt.clone()] {
        std::fs::File::open(&d).unwrap().set_modified(old).unwrap();
    }
}

#[test_id(trx3p6f)]
#[test]
fn a_podman_that_fails_to_list_images_is_asked_again_on_the_next_pass() {
    let _env = testkit::env_read();
    let dir = testkit::TempDir::new("untagged-failing");
    let root = dir.join("graph");
    std::fs::create_dir_all(root.join("overlay-images")).unwrap();
    std::fs::write(root.join("overlay-images/images.json"), "[]").unwrap();
    let asked = dir.join("asked");
    let podman = dir.join("podman");
    testkit::write_exe(&podman, &format!("#!/bin/sh\ncase \"$1\" in info) echo {};; *) echo \"$1\" >> {}; exit 1;; esac\n", root.display(), asked.display()));
    let seams = reconciler::Seams { podman: podman.display().to_string(), ..reconciler::Seams::bare() };
    let cfg = reconciler::Config::from_lookup(&|_| Ok("1".into()), seams, dir.join("run"), dir.path().display().to_string(), 0, String::new()).unwrap();
    for _ in 0..2 {
        let checks = reconciler::observe_untagged_images(&cfg);
        assert!(format!("{:?}", checks[0].raw).contains("Unobservable"), "{:?}", checks[0].raw);
    }
    assert_eq!(std::fs::read_to_string(&asked).unwrap().lines().count(), 2, "a failed listing is not held");
}

#[test_id(t2wpptx)]
#[test]
fn a_store_with_no_images_file_asks_podman_on_every_pass() {
    let _env = testkit::env_read();
    let dir = testkit::TempDir::new("untagged-no-file");
    let root = dir.join("graph");
    std::fs::create_dir_all(&root).unwrap();
    let asked = dir.join("asked");
    let podman = dir.join("podman");
    testkit::write_exe(&podman, &format!("#!/bin/sh\ncase \"$1\" in info) echo {};; *) echo \"$1\" >> {}; echo sha;; esac\n", root.display(), asked.display()));
    let seams = reconciler::Seams { podman: podman.display().to_string(), ..reconciler::Seams::bare() };
    let cfg = reconciler::Config::from_lookup(&|_| Ok("1".into()), seams, dir.join("run"), dir.path().display().to_string(), 0, String::new()).unwrap();
    for _ in 0..2 {
        reconciler::observe_untagged_images(&cfg);
    }
    assert_eq!(std::fs::read_to_string(&asked).unwrap().lines().count(), 2, "with no images.json there is nothing to key a held count on");
}
