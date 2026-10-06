use spira_sim::world::{down, up, Steps, PRODUCTION_LOCATORS};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Fake {
    ups: AtomicUsize,
    downs: AtomicUsize,
    lifecycles: AtomicUsize,
}

impl Steps for Fake {
    fn release(&self, _: &Path, _: &str, cache: &Path) -> Result<PathBuf, String> {
        let d = cache.join("rel");
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        Ok(d)
    }
    fn db_up(&self, world: &Path) -> Result<String, String> {
        self.ups.fetch_add(1, Ordering::SeqCst);
        let f = world.join("db/fx");
        std::fs::create_dir_all(&f).map_err(|e| e.to_string())?;
        Ok(f.display().to_string())
    }
    fn lifecycle(&self, _: &Path, fixture: &str, lifecycle: &Path, _: &Path) -> Result<(), String> {
        assert!(Path::new(fixture).is_dir() && lifecycle.ends_with("work/lifecycle"));
        self.lifecycles.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn db_down(&self, _: &str) -> Result<(), String> {
        self.downs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn config_set(&self, _: &Path, file: &Path, key: &str, value: &str) -> Result<(), String> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(file).map_err(|e| e.to_string())?;
        writeln!(f, "{key}={value}").map_err(|e| e.to_string())
    }
}

fn clean(_: &str) -> Option<String> {
    None
}

#[test]
fn up_then_down_leaves_nothing_behind() {
    let t = testkit::TempDir::new("simw"); let dir = t.join("world");
    let fake = Fake::default();
    up(fixture_repo().path(), &dir, "HEAD", &clean, &fake).unwrap();
    let work = dir.join("work");
    for p in ["release", "origin.git", "work", "run", "config/sim.toml", "bin/gh", "gh"] {
        assert!(dir.join(p).exists(), "{p}");
    }
    let rev = |r: &str| {
        let o = std::process::Command::new("git").arg("-C").arg(&work).args(["rev-parse", r]).output().unwrap();
        String::from_utf8(o.stdout).unwrap()
    };
    assert_eq!(rev("local/main"), rev("origin/main"));
    assert_eq!(rev("local/main"), rev("main"));
    let cfg = std::fs::read_to_string(dir.join("config/sim.toml")).unwrap();
    assert!(cfg.contains("repo.sim.mode=queue.local") && cfg.contains("spira.lifecycle_enforce=true"));
    assert!(cfg.contains(&format!("spira.gh={}", dir.join("bin/gh").display())));
    assert!(std::fs::read_to_string(dir.join("config/sim.env")).unwrap().contains("SIM_GH_DIR="));
    down(&dir, &fake).unwrap();
    assert!(!dir.exists());
    assert_eq!(fake.downs.load(Ordering::SeqCst), 1);
    assert_eq!(fake.lifecycles.load(Ordering::SeqCst), 1);
}

#[test]
fn up_refuses_each_production_locator_and_builds_nothing() {
    for key in PRODUCTION_LOCATORS {
        let t = testkit::TempDir::new("simw"); let dir = t.join("world");
        let fake = Fake::default();
        let env = |k: &str| (k == *key).then(|| "/prod/x".to_string());
        let e = up(fixture_repo().path(), &dir, "HEAD", &env, &fake).unwrap_err();
        assert!(e.contains(key), "{e}");
        assert!(!dir.exists(), "{key}: refusal left a directory");
        assert_eq!(fake.ups.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn a_failed_up_cleans_up_after_itself() {
    struct NoDb;
    impl Steps for NoDb {
        fn release(&self, r: &Path, t: &str, c: &Path) -> Result<PathBuf, String> {
            Fake::default().release(r, t, c)
        }
        fn db_up(&self, _: &Path) -> Result<String, String> {
            Err("no dolt".into())
        }
        fn db_down(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn lifecycle(&self, _: &Path, _: &str, _: &Path, _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn config_set(&self, _: &Path, _: &Path, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
    }
    let t = testkit::TempDir::new("simw"); let dir = t.join("world");
    assert!(up(fixture_repo().path(), &dir, "HEAD", &clean, &NoDb).is_err());
    assert!(!dir.exists());
}

#[test]
fn down_refuses_a_directory_that_is_not_a_world() {
    let t = testkit::TempDir::new("simw");
    let dir = t.join("w");
    std::fs::create_dir_all(&dir).unwrap();
    assert!(down(&dir, &Fake::default()).is_err());
    assert!(dir.exists());
}

fn fixture_repo() -> testkit::TempDir {
    let d = testkit::TempDir::new("simsrc");
    std::fs::write(d.join("a"), "a").unwrap();
    for args in [vec!["init", "-q"], vec!["add", "-A"], vec!["commit", "-q", "-m", "s"]] {
        let st = std::process::Command::new("git")
            .arg("-C").arg(d.path()).args(args)
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t.invalid")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t.invalid")
            .status().unwrap();
        assert!(st.success());
    }
    d
}
