//! With no ref moved, the scheduler's per-minute tasks start no git; a moved ref wakes only the
//! reads that depend on it. This binary sets PATH, so it is its own process.

#[macro_use]
extern crate spira_test_id;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

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
    commit(&local, "sp-aaaaaa landed", LANDED_S);
    git(&local, &["branch", "local/main"], 1);

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
    let home = spira_config::resolve::locate_home_for_process().unwrap();
    let modes = tasks::all().map(|t| format!("{}=act", t.name)).collect::<Vec<_>>().join(" ");
    let toml = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_ID_PREFIX", "sp"), ("SPIRA_BATCHER_FORGE_ENABLE", "1"), ("SPIRA_LC_SOCKET", "fixture/run/lc.sock"), ("SPIRA_TASK_MODES", &modes), ("SPIRA_REPO_MAP", map.to_str().unwrap())]);
    let lc = dir.join("spira-lc");
    testkit::write_exe(&lc, "#!/bin/sh\nexit 1\n");
    let env = testkit::env(&[
        ("PATH", Some(&path)),
        ("SPIRA_LC_BIN", lc.to_str()),
        ("SPIRA_TOML", toml.to_str()),
        ("SPIRA_HOME", home.to_str()),
        ("SPIRA_REPO_MAP", map.to_str()),
        ("SPIRA_HOME_REPO", Some("spira")),
    ]);
    let world = spira_runtime::world::World { repo_map: map, home_repo: "spira".into(), run_dir, ..Default::default() };
    let effects = Arc::new(spira_runtime::Effects { world, ..Default::default() });
    Fixture { _env: env, _dir: dir, log, local, pr, effects }
}

fn doubles(f: &Fixture) -> Arc<sim::doubles::Doubles> {
    let mut d = sim::doubles::service();
    for name in ["watch.throughput.observe", "landing.observe"] {
        let e = f.effects.clone();
        d = d.double(name, move |input| {
            use runtime::host::EffectRunner;
            e.run("k", &runtime::EffectSpec::keyed(name, "k", input.to_string(), 60_000))
        });
    }
    Arc::new(d)
}

fn run_hours(f: &Fixture, until: &[u64], between: &mut dyn FnMut(u64)) {
    let mut sim = Sim::new(1);
    for r in registrations(&spira_runtime::Settings::load().expect("settings")) {
        sim.add_workflow(r);
    }
    sim.set_effects(doubles(f));
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
    run_hours(&f, &[warm, warm + HOUR, warm + HOUR + 3 * MINUTE, warm + HOUR + 6 * MINUTE], &mut |t| match t {
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
