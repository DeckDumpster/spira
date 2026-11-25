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
const START: u64 = 1_700_000_000_000;
const LANDED_S: u64 = START / 1000 - 1_000;

struct Fixture {
    _env: testkit::EnvGuard,
    _dir: testkit::TempDir,
    log: PathBuf,
    local: PathBuf,
    pr: PathBuf,
    forge: PathBuf,
    wiki: PathBuf,
    seen: Arc<Mutex<Vec<(String, String)>>>,
    programs: PathBuf,
    started: PathBuf,
    sock: PathBuf,
    requests: Arc<Mutex<Vec<String>>>,
    refused: Arc<Mutex<Vec<String>>>,
    claimable: Arc<Mutex<String>>,
    ready: Arc<Mutex<Option<String>>>,
    effects: Arc<spira_runtime::Effects>,
    _serial: std::sync::MutexGuard<'static, ()>,
}

fn real_git() -> PathBuf {
    static REAL: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    REAL.get_or_init(|| std::env::split_paths(&std::env::var_os("PATH").unwrap()).map(|d| d.join("git")).find(|g| g.is_file()).expect("a git on PATH")).clone()
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
    for d in [release.join("bin"), release.join("spira"), bin.clone()] {
        std::fs::create_dir_all(&d).unwrap();
        for name in LOGGED_PROGRAMS {
            testkit::write_exe(d.join(name), &format!("#!/bin/sh\necho \"{name} $*\" >> {}\necho '[]'\n", started.display()));
        }
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
    std::thread::spawn(move || runtime::socket::answer(&listener, &|argv| {
        asked.lock().unwrap().push(argv.join(" "));
        match argv.first().map(String::as_str) {
            Some("rc") => Ok("{\"ok\":{\"list_rev\":1,\"items\":[]}}".into()),
            Some("list") => Ok(bead.lock().unwrap().as_deref().map_or("[]".into(), |id| format!("[{}]", lc_row(id)))),
            Some("plan") if argv.get(1).map(String::as_str) == Some("items") => Ok(format!("{{\"items\":[{}]}}", bead.lock().unwrap().as_deref().map_or(String::new(), plan_row))),
            Some("plan") => Ok("{\"watermark\":1,\"items\":[]}".into()),
            Some("content") => Ok("[]".into()),
            Some("list-asks") => Ok("[]".into()),
            Some("ops-graph" | "ops-view" | "section" | "audits" | "reconcile-content") => Ok("{}".into()),
            Some("git-fact") => Ok("{}".into()),
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
    let (wiki_s, prod_s, registry_s) = (wiki.display().to_string(), acc.join("spira").display().to_string(), format!("file://{}", kit.display()));
    let toml = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_RUN", &run_s), ("SPIRA_RUNES_DIR", &runes_s), ("SPIRA_WIKI", &wiki_s), ("SPIRA_PROD", &prod_s), ("SPIRA_KIT_REGISTRY", &registry_s), ("SPIRA_KIT_CRATE", "dumpster-kit"), ("SPIRA_KIT_SITES", "spira:Cargo.toml"), ("SPIRA_ID_PREFIX", "sp"), ("SPIRA_BATCHER_FORGE_ENABLE", "1"), ("SPIRA_LC_SOCKET", sock.to_str().unwrap()), ("SPIRA_TASK_MODES", &modes), ("SPIRA_REPO_MAP", map.to_str().unwrap())]);
    let programs = dir.join("programs.log");
    let trap = format!("#!/bin/sh\necho \"spira-lc $*\" >> {}\nexit 1\n", programs.display());
    testkit::write_exe(release.join("bin/spira-lc"), &trap);
    testkit::write_exe(bin.join("spira-lc"), &trap);
    let env = testkit::env(&[
        ("PATH", Some(&path)),
        ("SPIRA_LC_BIN", None),
        ("SPIRA_RELEASE", release.to_str()),
        ("SPIRA_UNITS", Some("scripted")),
        ("SPIRA_TOML", toml.to_str()),
        ("SPIRA_HOME", home.to_str()),
        ("SPIRA_REPO_MAP", map.to_str()),
        ("SPIRA_HOME_REPO", Some("spira")),
    ]);
    let world = spira_runtime::world::World { lc_socket: sock.clone(), repo_map: map, home_repo: "spira".into(), run_dir, wiki: wiki.clone(), ..Default::default() };
    let effects = Arc::new(spira_runtime::Effects { world, units: Some(Arc::new(runtime::units::ScriptedUnits::new(Arc::new(runtime::ports::WallClock)))), ..Default::default() });
    Fixture { _serial: serial, _env: env, _dir: dir, log, local, pr, forge, wiki, seen: Arc::default(), programs, started, sock: sock.clone(), requests, refused, claimable, ready: ready_id, effects }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.sock);
    }
}

fn process_socket() -> PathBuf {
    static HOME: std::sync::OnceLock<testkit::TempDir> = std::sync::OnceLock::new();
    let sock = HOME.get_or_init(|| testkit::TempDir::new("git-idle-sock")).join("lc.sock");
    let _ = std::fs::remove_file(&sock);
    sock
}

fn lc_row(id: &str) -> String {
    format!("{{\"bead_id\":\"{id}\",\"state\":\"READY\",\"holds\":[],\"stack\":{{}},\"version\":\"1\"}}")
}

fn plan_row(id: &str) -> String {
    format!("{{\"id\":\"{id}\",\"title\":\"t\",\"priority\":2,\"issue_type\":\"task\",\"status\":\"open\",\"labels\":[\"spira\",\"plan\"],\"created_at\":\"2020-01-01T00:00:00Z\",\"updated_at\":\"2020-01-01T00:00:00Z\"}}")
}

const LOGGED_PROGRAMS: [&str; 2] = ["spira-claim", "systemctl"];

fn logged_programs(f: &Fixture) -> Vec<String> {
    let all = std::fs::read_to_string(&f.started).unwrap_or_default();
    all.lines().filter(|l| l.starts_with("spira-claim ") || l.contains(" list-units")).map(str::to_string).collect()
}

fn assert_stubs_log(f: &Fixture) {
    let quiet = |p: &str, args: &[&str]| Command::new(p).args(args).stdout(std::process::Stdio::null()).status().unwrap();
    quiet("spira-claim", &["ready-count", "control"]);
    quiet("systemctl", &["--user", "list-units", "control"]);
    assert_eq!(logged_programs(f).len(), 2, "the logging stubs are what PATH resolves, so a started claim or listing is seen");
    std::fs::remove_file(&f.started).unwrap();
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
    let (mut at_warm, mut at_idle, mut at_commit, mut at_branch) = (0, 0, 0, 0);
    let (mut programs_at_warm, mut programs_at_idle, mut programs_at_ready, mut idle_passes, mut rows_before_ready) = (0, Vec::new(), Vec::new(), 0, Vec::new());
    let names = ["watch.throughput.observe", "landing.observe"];
    let sentinel = ["sentinel-pass.observe", "cockpit-tier-60s.observe"];
    run_hours(&f, &names, (&sentinel, warm + HOUR - 10 * MINUTE), &[warm, warm + HOUR, warm + HOUR + 3 * MINUTE, warm + HOUR + 6 * MINUTE], &mut |t| match t {
        t if t == warm => (at_warm, programs_at_warm) = (logged(&f).len(), logged_programs(&f).len()),
        t if t == warm + HOUR => {
            at_idle = logged(&f).len();
            programs_at_idle = logged_programs(&f);
            idle_passes = f.seen.lock().unwrap().iter().filter(|(n, _)| n == "sentinel-pass.observe").count();
            rows_before_ready = strand_rows(&f);
            make_ready(&f, "sp-zzzzzz");
            commit(&f.local, "sp-cccccc landed", LANDED_S + 10);
            git(&f.local, &["branch", "-f", "local/main"], 1);
        }
        t if t == warm + HOUR + 3 * MINUTE => {
            at_commit = logged(&f).len();
            programs_at_ready = logged_programs(&f);
            git(&f.pr, &["branch", "spira/sp-x"], 1);
        }
        _ => at_branch = logged(&f).len(),
    });
    assert_only_the_double_answered(&f);
    let all = logged(&f);
    assert_eq!(at_idle, at_warm, "an idle virtual hour logged git:\n{}", all[at_warm..at_idle].join("\n"));
    assert!(idle_passes >= 5, "the sentinel pass observed {idle_passes} times by the end of the idle hour");
    assert_eq!(programs_at_idle.len(), programs_at_warm, "an idle hour started programs:\n{}", programs_at_idle[programs_at_warm..].join("\n"));
    assert!(!rows_before_ready.iter().any(|r| r.contains("sp-zzzzzz")), "nothing is ready yet: {rows_before_ready:?}");
    let rows = strand_rows(&f);
    assert!(rows.iter().any(|r| r.contains("sp-zzzzzz")), "a bead made READY shows in the next strand rows: {rows:?}");
    assert_eq!(programs_at_ready.len(), programs_at_warm, "and reading it started no program: {programs_at_ready:?}");
    let local = f.local.display().to_string();
    let pr = f.pr.display().to_string();
    let after_commit: Vec<&String> = all[at_idle..at_commit].iter().filter(|l| !l.ends_with("rev-parse --verify --quiet refs/heads/spira/sp-zzzzzz")).collect();
    let logs = |lines: &[&String], repo: &str| lines.iter().filter(|l| l.contains(&format!("-C {repo} log "))).count();
    assert_eq!(logs(&after_commit, &local), 1, "a moved landing ref is read once: {after_commit:?}");
    assert_eq!(after_commit.len(), 1, "and nothing else asks git: {after_commit:?}");
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
    fn dir_path(&self) -> &Path {
        self._dir.path()
    }
}
