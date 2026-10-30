//! For one fixture wiki checkout, brain-dirty.sh and the brain-dirty handler leave the same rows
//! in the Concierge inbox. The script is the oracle and runs unmodified; both sides see a real
//! git repository, so the porcelain parse and the mtimes are the real ones.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use runtime::host::EffectRunner;
use runtime::ports::VirtualClock;
use runtime::{Engine, Executor, MemJournal, Outcome};
use spira_runtime::watchers::brain_dirty::BrainDirty;
use spira_runtime::world::World;
use spira_runtime::Effects;

const AGE: u64 = 1800;
const HOUR: u64 = 3600;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn age_file(path: &Path, secs: u64) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(secs)).unwrap();
}

struct Fixture {
    dir: testkit::TempDir,
}

impl Fixture {
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn build(name: &str, repo: bool) -> Fixture {
        let f = Fixture { dir: testkit::TempDir::new(&format!("brain-dirty-{name}")) };
        fs::create_dir_all(f.p("run/watchd")).unwrap();
        fs::create_dir_all(f.p("brain/wiki/notes")).unwrap();
        if repo {
            let brain = f.p("brain");
            git(&brain, &["init", "-q"]);
            for n in ["seed.md", "edited.md", "gone.md", "moved.md", "fresh-edit.md"] {
                fs::write(brain.join("wiki/notes").join(n), n).unwrap();
            }
            git(&brain, &["add", "-A"]);
            git(&brain, &["commit", "-qm", "init"]);
        }
        f
    }

    fn dirty_world(&self) {
        let notes = self.p("brain/wiki/notes");
        fs::write(notes.join("old new.md"), "x").unwrap();
        age_file(&notes.join("old new.md"), 2 * HOUR);
        fs::write(notes.join("fresh.md"), "x").unwrap();
        fs::write(notes.join("edited.md"), "changed").unwrap();
        age_file(&notes.join("edited.md"), 3 * HOUR);
        fs::write(notes.join("fresh-edit.md"), "changed").unwrap();
        fs::remove_file(notes.join("gone.md")).unwrap();
        git(&self.p("brain"), &["mv", "wiki/notes/moved.md", "wiki/notes/renamed.md"]);
        age_file(&notes.join("renamed.md"), 4 * HOUR);
        fs::create_dir_all(notes.join("deep/er")).unwrap();
        fs::write(notes.join("deep/er/leaf.md"), "x").unwrap();
        age_file(&notes.join("deep/er/leaf.md"), 5 * HOUR);
    }
}

fn debug_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().to_path_buf()
}

fn rows(inbox: &Path) -> Vec<String> {
    fs::read_to_string(inbox).unwrap_or_default().lines().map(|l| l.split_once(' ').unwrap().1.to_string()).collect()
}

fn old_rows(f: &Fixture, ticks: usize) -> Vec<String> {
    assert!(debug_dir().join("spira-config").exists(), "build spira-config first: brain-dirty.sh resolves its settings through it");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/brain-dirty.sh");
    let path = format!("{}:{}:/usr/bin:/bin", f.p("bin").display(), debug_dir().display());
    let layer = format!("[spira]\nrun = \"{}\"\nwiki = \"{}\"\nbrain_dirty_age = {AGE}\nconcierge_inbox = \"{}\"\n", f.p("run").display(), f.p("brain").display(), f.p("old-inbox").display());
    fs::write(f.p("layer.toml"), layer).unwrap();
    let out = Command::new("bash")
        .arg(&script)
        .args(["watch", "--interval", "0", "--ticks", &ticks.to_string()])
        .env_clear()
        .env("PATH", path)
        .env("HOME", f.dir.path())
        .env("SPIRA_TOML", format!("{}:{}", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira-config/tests/fixtures/complete.toml").display(), f.p("layer.toml").display()))
        .output()
        .unwrap();
    assert!(out.status.success(), "brain-dirty.sh failed: {}", String::from_utf8_lossy(&out.stderr));
    rows(&f.p("old-inbox"))
}

fn new_rows(f: &Fixture, ticks: usize) -> Vec<String> {
    let world = World { inbox: f.p("new-inbox"), wiki: f.p("brain"), ..World::default() };
    let fx = Effects { world, ..Effects::default() };
    let now_ms = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_millis() as u64;
    let clock = VirtualClock::new(now_ms);
    let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), vec![BrainDirty { interval_ms: 1000, age_secs: AGE }.registration()], 0).unwrap();
    let (id, stream, kind) = BrainDirty::boot();
    e.submit(id, stream, kind).unwrap();
    for _ in 0..ticks {
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
    rows(&f.p("new-inbox"))
}

fn same_rows(name: &str, repo: bool, ticks: usize, world: impl Fn(&Fixture)) -> Vec<String> {
    let f = Fixture::build(name, repo);
    world(&f);
    let old = old_rows(&f, ticks);
    let g = Fixture::build(name, repo);
    world(&g);
    let new = new_rows(&g, ticks);
    let strip = |rows: Vec<String>, f: &Fixture| rows.into_iter().map(|r| r.replace(&f.p("brain").display().to_string(), "WIKI")).collect::<Vec<_>>();
    let (old, new) = (strip(old, &f), strip(new, &g));
    assert_eq!(new, old, "scenario {name}: the handler and brain-dirty.sh disagree");
    old
}

#[test]
fn stale_paths_are_alarmed_once_and_fresh_ones_and_deletions_are_not() {
    let r = same_rows("stale", true, 2, Fixture::dirty_world);
    let named = |p: &str| r.iter().filter(|l| l.contains(&format!("BRAIN DIRTY: {p} has been"))).count();
    for p in ["wiki/notes/old new.md", "wiki/notes/edited.md", "wiki/notes/renamed.md", "wiki/notes/deep/er/leaf.md"] {
        assert_eq!(named(p), 1, "{p}: {r:?}");
    }
    for p in ["wiki/notes/fresh.md", "wiki/notes/fresh-edit.md", "wiki/notes/gone.md", "wiki/notes/moved.md"] {
        assert_eq!(named(p), 0, "{p}: {r:?}");
    }
    assert!(r.iter().any(|l| l.contains("uncommitted for 120 min; commit it by explicit path (git -C WIKI add -- wiki/notes/old new.md)")), "{r:?}");
}

#[test]
fn a_clean_tree_says_nothing() {
    assert!(same_rows("clean", true, 2, |_| {}).is_empty());
}

#[test]
fn a_wiki_that_is_not_a_repository_is_said_once() {
    let r = same_rows("notrepo", false, 2, |_| {});
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].contains("is not a git repository"), "{r:?}");
}

#[test]
fn an_unset_wiki_says_nothing_and_a_git_that_cannot_answer_is_not_a_clean_tree() {
    let g = Fixture::build("unset", true);
    g.dirty_world();
    let fx = Effects { world: World { inbox: g.p("inbox"), ..World::default() }, ..Effects::default() };
    let snap = fx.world.brain_dirty_snapshot().unwrap();
    assert!(!snap.contains("md"), "{snap}");
    let broken = World { wiki: g.p("brain"), ..World::default() };
    fs::write(g.p("brain/.git/index"), "garbage").unwrap();
    assert!(broken.brain_dirty_snapshot().is_err(), "a corrupt index must fail the observe, not read as clean");
}
