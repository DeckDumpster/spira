use spira_sim::world::{
    check_socket_path, db_root, down, is_world_serve, lc_pid_file, lc_socket, prebuilt_release, release_source, serve_argv, up, ProcessSteps,
    ReleaseSource, Steps, PRODUCTION_LOCATORS, RELEASE_ENV, TESTENV_ENV,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Default)]
struct Fake {
    releases: AtomicUsize,
    ups: AtomicUsize,
    downs: AtomicUsize,
    lifecycles: AtomicUsize,
    /// lc_serve / lc_stop / db_down, in order.
    calls: Mutex<Vec<String>>,
    serve_fails: bool,
}

impl Steps for Fake {
    fn release(&self, _: &Path, _: &str, cache: &Path) -> Result<PathBuf, String> {
        self.releases.fetch_add(1, Ordering::SeqCst);
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
        self.calls.lock().unwrap().push("db_down".into());
        Ok(())
    }
    fn config_set(&self, _: &Path, file: &Path, key: &str, value: &str) -> Result<(), String> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(file).map_err(|e| e.to_string())?;
        writeln!(f, "{key}={value}").map_err(|e| e.to_string())
    }
    fn lc_serve(&self, world: &Path, fixture: &str) -> Result<(), String> {
        assert!(Path::new(fixture).is_dir());
        // The config the serve reads is complete by the time it starts.
        assert!(std::fs::read_to_string(world.join("config/sim.toml")).unwrap().contains(&format!("spira.lc_socket={}", lc_socket(world).display())));
        std::fs::write(lc_pid_file(world), "424242").unwrap();
        self.calls.lock().unwrap().push("lc_serve".into());
        if self.serve_fails {
            return Err("serve did not answer".into());
        }
        Ok(())
    }
    fn lc_stop(&self, _: &Path, pid: u32) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("lc_stop {pid}"));
        Ok(())
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
    for p in ["release", "origin.git", "work", "run", "config/sim.toml", "bin/gh", "bin/round-vm", "gh"] {
        assert!(dir.join(p).exists(), "{p}");
    }
    let rev = |r: &str| {
        let o = std::process::Command::new("git").arg("-C").arg(&work).args(["rev-parse", r]).output().unwrap();
        String::from_utf8(o.stdout).unwrap()
    };
    assert_eq!(rev("local/main"), rev("origin/main"));
    assert_eq!(rev("local/main"), rev("main"));
    let cfg = std::fs::read_to_string(dir.join("config/sim.toml")).unwrap();
    assert!(cfg.contains(r#""mode":"queue.local""#) && cfg.contains(r#""base":"local/main""#), "{cfg}");
    assert!(cfg.contains(&format!(r#""path":"{}""#, work.display())), "{cfg}");
    assert!(!cfg.contains("lifecycle_enforce"), "{cfg}");
    assert!(cfg.contains(&format!("spira.gh={}", dir.join("bin/gh").display())));
    let env = std::fs::read_to_string(dir.join("config/sim.env")).unwrap();
    assert!(env.contains("SIM_GH_DIR="));
    let probe = env.lines().find_map(|l| l.strip_prefix("SIM_PROBE=")).expect("world up writes SIM_PROBE");
    assert_eq!(probe, spira_sim::world::probe_command(&std::env::current_exe().unwrap(), &dir.canonicalize().unwrap()));
    assert!(probe.ends_with(&format!(" probe '{}'", dir.canonicalize().unwrap().display())), "{probe}");
    assert!(env.contains(&format!("SIM_WORLD={}\n", dir.canonicalize().unwrap().display())), "{env}");
    assert_eq!(std::fs::read_to_string(dir.join("gate-verdict")).unwrap(), "green\n", "a world starts green");
    down(&dir, &fake).unwrap();
    assert!(!dir.exists());
    assert_eq!(fake.downs.load(Ordering::SeqCst), 1);
    assert_eq!(fake.lifecycles.load(Ordering::SeqCst), 1);
}

#[test]
fn up_starts_the_worlds_serve_and_down_stops_it_by_the_recorded_pid_first() {
    let t = testkit::TempDir::new("simw");
    let dir = t.join("world");
    let fake = Fake::default();
    up(fixture_repo().path(), &dir, "HEAD", &clean, &fake).unwrap();
    let dir = dir.canonicalize().unwrap();
    let sock = lc_socket(&dir);
    assert!(sock.starts_with(&dir), "the socket is under the world dir");
    let cfg = std::fs::read_to_string(dir.join("config/sim.toml")).unwrap();
    // The complete base: a process refuses a config that does not declare every key.
    assert!(cfg.contains("stack_max_depth"), "{cfg}");
    assert!(cfg.contains(&format!("spira.lc_socket={}", sock.display())), "{cfg}");
    assert!(cfg.contains(&format!("spira.lc_password_file={}", dir.join("config/lc-credential").display())), "{cfg}");
    assert!(cfg.contains(&format!("spira.run={}", dir.join("run").display())), "{cfg}");
    let env = std::fs::read_to_string(dir.join("config/sim.env")).unwrap();
    assert!(env.contains(&format!("SPIRA_LC_SOCKET={}\n", sock.display())), "{env}");
    assert!(env.contains(&format!("SPIRA_RELEASE={}\n", dir.join("release").display())), "{env}");
    assert_eq!(*fake.calls.lock().unwrap(), vec!["lc_serve"]);
    down(&dir, &fake).unwrap();
    assert_eq!(*fake.calls.lock().unwrap(), vec!["lc_serve", "lc_stop 424242", "db_down"]);
}

#[test]
fn a_serve_that_never_answers_fails_up_and_is_stopped() {
    let t = testkit::TempDir::new("simw");
    let dir = t.join("world");
    let fake = Fake { serve_fails: true, ..Fake::default() };
    let e = up(fixture_repo().path(), &dir, "HEAD", &clean, &fake).unwrap_err();
    assert!(e.contains("did not answer"), "{e}");
    assert!(!dir.exists());
    assert_eq!(*fake.calls.lock().unwrap(), vec!["lc_serve", "lc_stop 424242", "db_down"]);
}

#[test]
fn a_world_whose_socket_cannot_be_bound_is_refused_before_anything() {
    let t = testkit::TempDir::new("simw");
    let dir = t.join(&"w".repeat(120));
    let fake = Fake::default();
    let e = up(fixture_repo().path(), &dir, "HEAD", &clean, &fake).unwrap_err();
    assert!(e.contains("unix socket limit"), "{e}");
    assert!(!dir.exists());
    assert_eq!(fake.ups.load(Ordering::SeqCst), 0);
    assert!(check_socket_path(Path::new("/tmp/w")).is_ok());
}

#[test]
fn only_this_worlds_serve_command_line_matches() {
    let w = Path::new("/tmp/w1");
    let line = |args: &[&str]| args.iter().flat_map(|a| a.bytes().chain([0])).collect::<Vec<u8>>();
    assert!(is_world_serve(&line(&["/tmp/w1/release/bin/spira-lc", "serve", "/tmp/w1/lc.sock"]), w));
    assert_eq!(serve_argv(w), vec!["/tmp/w1/release/bin/spira-lc", "serve", "/tmp/w1/lc.sock"]);
    for other in [
        line(&["/tmp/w2/release/bin/spira-lc", "serve", "/tmp/w2/lc.sock"]), // another world
        line(&["/home/u/spira-releases/x/bin/spira-lc", "serve"]),            // production's
        line(&["/tmp/w1/release/bin/spira-lc", "serve", "/tmp/w1/lc.sock", "x"]),
        line(&["sleep", "30"]),
        Vec::new(), // a zombie's cmdline
    ] {
        assert!(!is_world_serve(&other, w), "{other:?}");
    }
}

#[test]
fn lc_stop_signals_this_worlds_serve_and_never_another_process() {
    let t = testkit::TempDir::new("simstop");
    let world = t.path().canonicalize().unwrap();
    // A stand-in serve: `<world>/release/bin/spira-lc serve <sock>` is /bin/sh running the
    // script `serve` from its cwd, so its cmdline is exactly the world's serve argv.
    std::fs::create_dir_all(world.join("release/bin")).unwrap();
    std::os::unix::fs::symlink("/bin/sh", world.join("release/bin/spira-lc")).unwrap();
    std::fs::write(world.join("serve"), "while :; do sleep 1; done\n").unwrap();
    let argv = serve_argv(&world);
    let mut serve = std::process::Command::new(&argv[0]).args(&argv[1..]).current_dir(&world).spawn().unwrap();
    let mut other = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    let alive = |pid: u32| std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z "));
    let t0 = std::time::Instant::now();
    while !std::fs::read(format!("/proc/{}/cmdline", serve.id())).is_ok_and(|c| is_world_serve(&c, &world)) {
        assert!(t0.elapsed().as_secs() < 5, "the stand-in serve never showed the serve cmdline");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // Recorded PID now names some other process: left alone.
    ProcessSteps.lc_stop(&world, other.id()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(alive(other.id()), "lc_stop signalled a process that is not the world's serve");

    ProcessSteps.lc_stop(&world, serve.id()).unwrap();
    assert!(!alive(serve.id()), "the world's serve survived lc_stop");
    // A PID that is already gone is not an error.
    ProcessSteps.lc_stop(&world, serve.id()).unwrap();

    let _ = serve.wait();
    let _ = other.kill();
    let _ = other.wait();
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
        fn lc_serve(&self, _: &Path, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn lc_stop(&self, _: &Path, _: u32) -> Result<(), String> {
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

/// A directory shaped like `spira-releases/<sha>`.
fn fake_release(t: &testkit::TempDir) -> PathBuf {
    let r = t.join("release-sha");
    std::fs::create_dir_all(r.join("bin")).unwrap();
    std::fs::create_dir_all(r.join("spira")).unwrap();
    for b in ["bin/spira-config", "bin/spira-lc"] {
        std::fs::write(r.join(b), "").unwrap();
    }
    r
}

fn env_of(pairs: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
    move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
}

#[test]
fn a_set_release_resolves_to_that_directory_and_nothing_else() {
    let t = testkit::TempDir::new("simrel");
    let r = fake_release(&t);
    let env = env_of(vec![(RELEASE_ENV, r.display().to_string())]);
    assert_eq!(release_source(&env).unwrap(), ReleaseSource::Prebuilt(r.canonicalize().unwrap()));
    // Inside testenv a set release is just as good: the refusal is for an unset one.
    let env = env_of(vec![(RELEASE_ENV, r.display().to_string()), (TESTENV_ENV, "1".into())]);
    assert_eq!(release_source(&env).unwrap(), ReleaseSource::Prebuilt(r.canonicalize().unwrap()));
}

#[test]
fn an_unset_release_builds_on_the_host_only() {
    assert_eq!(release_source(&clean).unwrap(), ReleaseSource::Build);
    // An empty value is unset, not a release at "".
    let env = env_of(vec![(RELEASE_ENV, String::new())]);
    assert_eq!(release_source(&env).unwrap(), ReleaseSource::Build);
}

#[test]
fn inside_testenv_an_unset_release_is_refused_and_names_the_variable() {
    for v in [None, Some(String::new())] {
        let mut pairs = vec![(TESTENV_ENV, "1".to_string())];
        if let Some(v) = v {
            pairs.push((RELEASE_ENV, v));
        }
        let e = release_source(&env_of(pairs)).unwrap_err();
        assert!(e.contains(RELEASE_ENV) && e.contains("never builds"), "{e}");
    }
}

#[test]
fn a_directory_that_is_not_a_release_is_refused_naming_what_is_missing() {
    let t = testkit::TempDir::new("simrel");
    let r = fake_release(&t);
    std::fs::remove_file(r.join("bin/spira-lc")).unwrap();
    assert!(prebuilt_release(&r).unwrap_err().contains("bin/spira-lc"));
    std::fs::remove_dir_all(r.join("spira")).unwrap();
    assert!(prebuilt_release(&r).unwrap_err().contains("no spira/"));
    let e = prebuilt_release(&t.join("absent")).unwrap_err();
    assert!(e.contains(RELEASE_ENV), "{e}");
}

#[test]
fn up_with_a_prebuilt_release_links_it_and_builds_nothing() {
    let t = testkit::TempDir::new("simw");
    let r = fake_release(&t);
    let dir = t.join("world");
    let fake = Fake::default();
    let env = env_of(vec![(RELEASE_ENV, r.display().to_string()), (TESTENV_ENV, "1".into())]);
    up(fixture_repo().path(), &dir, "HEAD", &env, &fake).unwrap();
    assert_eq!(fake.releases.load(Ordering::SeqCst), 0, "a prebuilt release was built anyway");
    assert_eq!(std::fs::read_link(dir.join("release")).unwrap(), r.canonicalize().unwrap());
    assert!(!dir.join("cache").exists(), "a prebuilt world made a build cache");
    down(&dir, &fake).unwrap();
    assert!(r.join("bin/spira-lc").is_file(), "down removed the prebuilt release");
}

#[test]
fn up_inside_testenv_without_a_release_refuses_before_anything() {
    // The planted control for the refusal: the same up as above, SPIRA_SIM_RELEASE unset.
    let t = testkit::TempDir::new("simw");
    let dir = t.join("world");
    let fake = Fake::default();
    let env = env_of(vec![(TESTENV_ENV, "1".into())]);
    let e = up(fixture_repo().path(), &dir, "HEAD", &env, &fake).unwrap_err();
    assert!(e.contains(RELEASE_ENV), "{e}");
    assert!(!dir.exists(), "the refusal left a directory");
    assert_eq!(fake.releases.load(Ordering::SeqCst), 0);
    assert_eq!(fake.ups.load(Ordering::SeqCst), 0);
}

#[test]
fn the_db_root_is_testenvs_tmpfs_root_when_nested_and_the_worlds_own_otherwise() {
    let w = Path::new("/w");
    assert_eq!(db_root(w, &clean), PathBuf::from("/w/db"));
    assert_eq!(db_root(w, &env_of(vec![("TESTDB_ROOT", String::new())])), PathBuf::from("/w/db"));
    assert_eq!(db_root(w, &env_of(vec![("TESTDB_ROOT", "/tmp/spira-testdb".into())])), PathBuf::from("/tmp/spira-testdb"));
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
