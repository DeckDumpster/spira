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
    effects: Arc<spira_runtime::Effects>,
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

fn fixture() -> Fixture {
    let dir = testkit::TempDir::new("git-idle");
    let real = real_git();
    let log = dir.join("git.log");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    testkit::write_exe(bin.join("git"), &format!("#!/bin/sh\necho \"$PWD :: $*\" >> {}\nexec {} \"$@\"\n", log.display(), real.display()));
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
    let sock = dir.join("lc.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || runtime::socket::answer(&listener, &|argv| {
        match argv.first().map(String::as_str) {
            Some("rc") => Ok("{\"ok\":{\"list_rev\":1,\"items\":[]}}".into()),
            Some("content") | Some("list") => Ok("[]".into()),
            Some("git-fact") => Ok("{}".into()),
            _ => Err(runtime::RuntimeError::new("the double has no such verb")),
        }
    }));
    let home = spira_config::resolve::locate_home_for_process().unwrap();
    let modes = tasks::all().map(|t| format!("{}=act", t.name)).collect::<Vec<_>>().join(" ");
    let (wiki_s, prod_s, registry_s) = (wiki.display().to_string(), acc.join("spira").display().to_string(), format!("file://{}", kit.display()));
    let toml = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_WIKI", &wiki_s), ("SPIRA_PROD", &prod_s), ("SPIRA_KIT_REGISTRY", &registry_s), ("SPIRA_KIT_CRATE", "dumpster-kit"), ("SPIRA_KIT_SITES", "spira:Cargo.toml"), ("SPIRA_ID_PREFIX", "sp"), ("SPIRA_BATCHER_FORGE_ENABLE", "1"), ("SPIRA_LC_SOCKET", sock.to_str().unwrap()), ("SPIRA_TASK_MODES", &modes), ("SPIRA_REPO_MAP", map.to_str().unwrap())]);
    let lc = dir.join("spira-lc");
    testkit::write_exe(&lc, "#!/bin/sh\necho '[]'\n");
    let env = testkit::env(&[
        ("PATH", Some(&path)),
        ("SPIRA_LC_BIN", lc.to_str()),
        ("SPIRA_TOML", toml.to_str()),
        ("SPIRA_HOME", home.to_str()),
        ("SPIRA_REPO_MAP", map.to_str()),
        ("SPIRA_HOME_REPO", Some("spira")),
    ]);
    let world = spira_runtime::world::World { lc_socket: sock.clone(), repo_map: map, home_repo: "spira".into(), run_dir, wiki: wiki.clone(), ..Default::default() };
    let effects = Arc::new(spira_runtime::Effects { world, ..Default::default() });
    Fixture { _env: env, _dir: dir, log, local, pr, forge, wiki, seen: Arc::default(), effects }
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

fn run_hours(f: &Fixture, names: &[&'static str], until: &[u64], between: &mut dyn FnMut(u64)) {
    let mut sim = Sim::new(1);
    for r in registrations(&spira_runtime::Settings::load().expect("settings")) {
        sim.add_workflow(r);
    }
    sim.set_effects(doubles(f, names));
    for (_, stream, kind) in tasks::boots() {
        sim.boot_workflow(START, stream, &kind.tag, &kind.body);
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

#[test_id(tqoiwy1)]
#[test]
fn an_idle_hour_after_warm_up_logs_zero_git_and_a_moved_ref_wakes_only_its_own_reads() {
    let f = fixture();
    let warm = START + 10 * MINUTE;
    let (mut at_warm, mut at_idle, mut at_commit, mut at_branch) = (0, 0, 0, 0);
    run_hours(&f, &["watch.throughput.observe", "landing.observe"], &[warm, warm + HOUR, warm + HOUR + 3 * MINUTE, warm + HOUR + 6 * MINUTE], &mut |t| match t {
        t if t == warm => at_warm = logged(&f).len(),
        t if t == warm + HOUR => {
            at_idle = logged(&f).len();
            commit(&f.local, "sp-cccccc landed", LANDED_S + 10);
            git(&f.local, &["branch", "-f", "local/main"], 1);
        }
        t if t == warm + HOUR + 3 * MINUTE => {
            at_commit = logged(&f).len();
            git(&f.pr, &["branch", "spira/sp-x"], 1);
        }
        _ => at_branch = logged(&f).len(),
    });
    let all = logged(&f);
    assert_eq!(at_idle, at_warm, "an idle virtual hour logged git:\n{}", all[at_warm..at_idle].join("\n"));
    let local = f.local.display().to_string();
    let pr = f.pr.display().to_string();
    let after_commit: Vec<&String> = all[at_idle..at_commit].iter().collect();
    let logs = |lines: &[&String], repo: &str| lines.iter().filter(|l| l.contains(&format!("-C {repo} log "))).count();
    assert_eq!(logs(&after_commit, &local), 1, "a moved landing ref is read once: {after_commit:?}");
    assert_eq!(after_commit.len(), 1, "and nothing else asks git: {after_commit:?}");
    let counted: u64 = f.effects.world.landed_on_refs((warm + HOUR + 3 * MINUTE) / 1000, "sp").unwrap().iter().find(|r| r.repo == "spira").map(|r| r.hourly.iter().sum()).unwrap();
    assert_eq!(counted, 2, "landed_on_refs counts the new landing");
    let after_branch = &all[at_commit..at_branch];
    assert!(after_branch.iter().any(|l| l.contains(&format!("-C {pr} ")) && l.contains(" fetch ")), "a spira/* branch makes landing fetch: {after_branch:?}");
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
    let (warm, idle) = (START + HOUR, START + 2 * HOUR);
    let marks = [warm, idle, idle + 16 * MINUTE, idle + 32 * MINUTE, idle + 43 * MINUTE, idle + 54 * MINUTE];
    let (mut at_warm, mut at_idle) = (0, 0);
    let mut unsent_before = String::new();
    let second = f.dir_path().join("second");
    run_hours(&f, &SUB_HOUR, &marks, &mut |t| match t {
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
    let all = logged(&f);
    assert_eq!(at_idle, at_warm, "an idle virtual hour logged git: {}\n{}", argv_counts(&all[at_warm..at_idle]), all[at_warm..at_idle].join("\n"));
    assert_ne!(unsent_kv(&f), unsent_before, "a new spira/* branch changes the unsent section");
    assert!(!unsent_before.is_empty());
}

impl Fixture {
    fn dir_path(&self) -> &Path {
        self._dir.path()
    }
}
