//! For one fixture world of real git repositories, publish-backlog.sh and the publish-backlog
//! handler say the same lines. The script is the oracle and runs unmodified; the handler runs on
//! a virtual clock set to the wall clock the commits were dated against. Only ages are masked:
//! the two runs are a few seconds apart.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use runtime::host::EffectRunner;
use runtime::ports::VirtualClock;
use runtime::{Engine, Executor, MemJournal, Outcome};
use spira_runtime::watchers::publish_backlog::{PublishBacklog, REPO_MAP_KEY};
use spira_runtime::world::World;
use spira_runtime::Effects;

struct Fixture {
    dir: testkit::TempDir,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

impl Fixture {
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn git(&self, repo: &str, args: &[&str], date: Option<(u64, u64)>) {
        let mut c = Command::new("git");
        c.arg("-C").arg(self.p(repo)).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t");
        if let Some((authored, landed)) = date {
            c.env("GIT_AUTHOR_DATE", format!("@{authored} +0000")).env("GIT_COMMITTER_DATE", format!("@{landed} +0000"));
        }
        assert!(c.output().unwrap().status.success(), "git {args:?} failed");
    }

    /// A commit on local/main whose reflog entry carries `landed` seconds.
    fn land(&self, msg: &str, authored_ago: u64, landed_ago: u64) {
        fs::write(self.p("repo/f.txt"), format!("{msg}\n")).unwrap();
        self.git("repo", &["add", "f.txt"], None);
        self.git("repo", &["commit", "-q", "-m", msg], Some((now() - authored_ago, now() - landed_ago)));
    }

    fn publish(&self) {
        self.git("repo", &["push", "-q", "origin", "local/main:main"], None);
    }

    fn build(name: &str) -> Fixture {
        let f = Fixture { dir: testkit::TempDir::new(&format!("pb-{name}")) };
        for d in ["bin", "run", "forge"] {
            fs::create_dir_all(f.p(d)).unwrap();
        }
        fs::create_dir_all(f.p("repo")).unwrap();
        f.git("", &["init", "-q", "--bare", "-b", "main", "remote.git"], None);
        f.git("repo", &["init", "-q", "-b", "main"], None);
        f.git("repo", &["commit", "-q", "--allow-empty", "-m", "base"], None);
        f.git("repo", &["remote", "add", "origin", f.p("remote.git").to_str().unwrap()], None);
        f.git("repo", &["push", "-q", "origin", "main"], None);
        f.git("repo", &["branch", "local/main", "main"], None);
        f.git("repo", &["checkout", "-q", "local/main"], None);
        f.git("", &["init", "-q", "-b", "main", "frepo"], None);
        f.git("frepo", &["commit", "-q", "--allow-empty", "-m", "base"], None);
        fs::write(f.p("rows"), format!("fixlocal | {} | queue.local | local/main | | |\nfixforge | {} | queue | main | | |\n", f.p("repo").display(), f.p("frepo").display())).unwrap();
        fs::write(f.p("mail-out"), "").unwrap();
        testkit::write_exe(f.p("bin/mail"), &format!("#!/usr/bin/env bash\nwhile [ $# -gt 0 ]; do [ \"$1\" = --subject ] && printf '%s\\n' \"$2\" >> {}/mail-out; shift; done\ncat >/dev/null\n", f.dir.path().display()));
        f
    }

    /// A socket that answers each request by running the real `spira-lc` with the argv it was sent.
    fn serve_lc(&self) -> PathBuf {
        let sock = self.p("lc.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let (path, home, toml, repo) = (
            format!("{}:/usr/bin:/bin", debug_dir().display()),
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira"),
            format!("{}:{}", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/testlib/complete.toml").display(), self.p("layer.toml").display()),
            self.p("repo"),
        );
        let root = self.dir.path().to_path_buf();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let argv: Vec<String> = serde_json::from_str(&line).unwrap();
                let out = Command::new(debug_dir().join("spira-lc")).args(&argv).env_clear().env("PATH", &path).env("HOME", &root).env("SPIRA_TOML", &toml).env("SPIRA_HOME", &home).env("SPIRA_REPO", &repo).output().unwrap();
                let body = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
                let mut w = &stream;
                writeln!(w, "{{\"exit_code\": {}, \"len\": {}}}", out.status.code().unwrap(), body.len()).unwrap();
                w.write_all(body.as_bytes()).unwrap();
            }
        });
        sock
    }

    fn layer(&self, count: u64, age: u64) {
        fs::write(
            self.p("layer.toml"),
            format!("[spira]\nrun = \"{}\"\nrepo_map = \"{}\"\nhome_repo = \"fixlocal\"\nlocal_backlog_count = {count}\nlocal_backlog_age = {age}\n{}", self.p("run").display(), self.p("rows").display(), fs::read_to_string(self.p("layer-extra")).unwrap_or_default()),
        )
        .unwrap();
    }
}

fn debug_dir() -> PathBuf {
    std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_path_buf()
}

fn masked(line: &str) -> String {
    let Some(at) = line.find("oldest ") else { return line.to_string() };
    let rest = &line[at + 7..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    format!("{}oldest N{}", &line[..at], &rest[digits..])
}

struct Scenario {
    name: &'static str,
    count: u64,
    age: u64,
    ticks: usize,
    setup: fn(&Fixture),
    change: Option<fn(&Fixture)>,
}

const QUIET: Scenario = Scenario { name: "", count: 50, age: 10_800, ticks: 1, setup: |_| {}, change: None };

fn old_rows(f: &Fixture, s: &Scenario) -> Vec<String> {
    assert!(debug_dir().join("spira-config").exists(), "build spira-config first: publish-backlog.sh resolves its settings and the repo map through it");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/publish-backlog.sh");
    let path = format!("{}:{}:/usr/bin:/bin", f.p("bin").display(), debug_dir().display());
    let mut child = Command::new("bash")
        .arg(&script)
        .args(["watch", "--interval", "1", "--ticks", &s.ticks.to_string()])
        .env_clear()
        .env("PATH", path)
        .env("HOME", f.dir.path())
        .env("SPIRA_TOML", format!("{}:{}", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/testlib/complete.toml").display(), f.p("layer.toml").display()))
        .env("SPIRA_HOME", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira"))
        .env("SPIRA_REPO", f.p("repo"))
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    if let Some(change) = s.change {
        let first_tick = f.p("run/watchd/publish-backlog.health");
        let mut waited = 0;
        while !first_tick.exists() {
            assert!(waited < 2000, "publish-backlog.sh never finished its first tick in {}", s.name);
            std::thread::sleep(std::time::Duration::from_millis(5)); // clock-safe: caps the wait for the script's first-tick health file
            waited += 1;
        }
        change(f);
    }
    assert!(child.wait().unwrap().success(), "publish-backlog.sh failed in {}", s.name);
    fs::read_to_string(f.p("mail-out")).unwrap().lines().map(|l| masked(l.strip_prefix("publish-backlog: ").unwrap())).collect()
}

fn new_rows(f: &Fixture, s: &Scenario) -> Vec<String> {
    let world = World { run_dir: f.p("run"), inbox: f.p("new-inbox"), repo_map: f.p("rows"), lc_socket: f.serve_lc(), ..World::default() };
    let fx = Effects { world, ..Effects::default() };
    let clock = VirtualClock::new(now() * 1000);
    let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), vec![PublishBacklog { interval_ms: 1000, max_count: s.count, max_age_secs: s.age }.registration()], 0).unwrap();
    let (id, stream, kind) = PublishBacklog::boot();
    e.submit(id, stream, kind).unwrap();
    for tick in 0..s.ticks {
        if tick == 1 {
            if let Some(change) = s.change {
                change(f);
            }
        }
        e.run_until_idle().unwrap();
        loop {
            let pending = e.pending_effects();
            if pending.is_empty() {
                break;
            }
            for p in pending {
                let out = match fx.run(&p.key, &p.spec) {
                    Ok(d) => Outcome::Succeeded(d),
                    Err(d) => Outcome::Failed(d),
                };
                e.complete(&p.key, out).unwrap();
            }
            e.run_until_idle().unwrap();
        }
        clock.advance(1000);
    }
    fs::read_to_string(f.p("new-inbox")).unwrap_or_default().lines().map(|l| masked(l.split_once("[watch:publish-backlog] ").unwrap().1)).collect()
}

fn same_rows(s: Scenario) -> Vec<String> {
    let f = Fixture::build(s.name);
    (s.setup)(&f);
    f.layer(s.count, s.age);
    let old = old_rows(&f, &s);
    let g = Fixture::build(s.name);
    (s.setup)(&g);
    g.layer(s.count, s.age);
    let new = new_rows(&g, &s);
    assert_eq!(new, old, "scenario {}: the handler and publish-backlog.sh disagree", s.name);
    old
}

#[test]
fn e2e_a_backlog_under_both_thresholds_is_quiet() {
    assert!(same_rows(Scenario { name: "quiet", setup: |f| f.land("c1", 5, 5), ..QUIET }).is_empty());
}

#[test]
fn e2e_crossing_the_count_threshold_is_announced_once_however_long_it_stays() {
    let r = same_rows(Scenario { name: "count", count: 0, ticks: 3, setup: |f| f.land("c1", 5, 5), ..QUIET });
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].starts_with("OVER fixlocal: 1 unpublished commit(s)"), "{r:?}");
}

#[test]
fn e2e_catching_the_forge_up_clears_once() {
    let r = same_rows(Scenario { name: "clear", count: 0, ticks: 3, setup: |f| f.land("c1", 5, 5), change: Some(Fixture::publish), ..QUIET });
    assert!(r[0].starts_with("OVER fixlocal") && r[1].starts_with("CLEAR fixlocal: back under threshold (0 unpublished, oldest N"), "{r:?}");
    assert_eq!(r.len(), 2, "{r:?}");
}

#[test]
fn e2e_an_unreadable_repo_map_is_blind_once_and_nothing_else() {
    let f = Fixture::build("nomap");
    fs::remove_file(f.p("rows")).unwrap();
    let s = Scenario { name: "nomap", ticks: 2, ..QUIET };
    let r = new_rows(&f, &s);
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].starts_with(&format!("BLIND {}: the backlog watcher cannot measure — the repo map is unreadable", REPO_MAP_KEY)), "{r:?}");
}

#[test]
fn e2e_an_old_arrival_alone_crosses() {
    let r = same_rows(Scenario { name: "age", count: 1000, age: 60, setup: |f| f.land("old", 90_000, 7_200), ..QUIET });
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].starts_with("OVER fixlocal: 1 unpublished commit(s), oldest N"), "{r:?}");
}

#[test]
fn e2e_age_is_when_the_commit_reached_the_landing_ref_not_when_it_was_authored() {
    assert!(same_rows(Scenario { name: "fresh", count: 1000, age: 10_800, setup: |f| f.land("authored long ago", 43_200, 60), ..QUIET }).is_empty());
    let r = same_rows(Scenario { name: "stale", count: 1000, age: 10_800, setup: |f| f.land("landed long ago", 43_200, 14_400), ..QUIET });
    assert_eq!(r.len(), 1, "{r:?}");
}

#[test]
fn e2e_a_repo_that_publishes_on_merge_is_never_named() {
    let r = same_rows(Scenario { name: "forge", count: 0, age: 0, setup: |f| f.land("c1", 5, 5), ..QUIET });
    assert!(r.iter().all(|l| !l.contains("fixforge")), "{r:?}");
    assert_eq!(r.len(), 1, "{r:?}");
}

#[test]
fn e2e_a_landing_ref_whose_arrival_cannot_be_read_is_blind_once_never_clear() {
    let r = same_rows(Scenario { name: "blind", ticks: 2, setup: |f| { f.land("c1", 5, 5); fs::remove_file(f.p("repo/.git/logs/refs/heads/local/main")).unwrap(); }, ..QUIET });
    assert_eq!(r, ["BLIND fixlocal: the backlog watcher cannot measure — the unpublished range could not be computed"]);
}

#[test]
fn a_multi_commit_backlog_is_counted_commit_by_commit_and_ages_from_its_oldest() {
    let r = same_rows(Scenario { name: "multi", count: 2, age: 10_800, setup: |f| { f.land("c1", 400, 300); f.land("c2", 200, 200); f.land("c3", 100, 100); }, ..QUIET });
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].starts_with("OVER fixlocal: 3 unpublished commit(s), oldest N"), "{r:?}");
}

#[test]
fn partly_publishing_a_multi_commit_backlog_still_counts_what_is_left() {
    let r = same_rows(Scenario { name: "partial", count: 1, ticks: 3, setup: |f| { f.land("c1", 400, 300); f.land("c2", 200, 200); f.land("c3", 100, 100); }, change: Some(|f| f.git("repo", &["push", "-q", "origin", "local/main~1:main"], None)), ..QUIET });
    assert!(r[0].starts_with("OVER fixlocal: 3 unpublished commit(s)"), "{r:?}");
    assert!(r[1].starts_with("CLEAR fixlocal: back under threshold (1 unpublished, oldest N"), "{r:?}");
}

#[test]
fn a_repo_declared_to_publish_nowhere_is_skipped_never_blind() {
    let r = same_rows(Scenario {
        name: "nowhere",
        count: 0,
        age: 0,
        setup: |f| {
            f.land("c1", 5, 5);
            fs::write(f.p("layer-extra"), "[publish.fixlocal]\nremote = \"none\"\n").unwrap();
            f.git("repo", &["remote", "remove", "origin"], None);
        },
        ..QUIET
    });
    assert!(r.is_empty(), "{r:?}");
}
