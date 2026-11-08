use spira_sim::world::{
    control_store, db_root, down, is_world_serve, lc_pid_file, lc_socket, release_source, serve_argv,
    ReleaseSource, Steps, RELEASE_ENV, TESTENV_ENV,
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
    fn bd(&self) -> Result<PathBuf, String> {
        Ok(PathBuf::from("/fake/bin/bd"))
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
    fn control_store(&self, _: &Path, world: &Path) -> Result<(), String> {
        // The store is made from the world's complete config, before the serve that opens it.
        let cfg = std::fs::read_to_string(world.join("config/sim.toml")).unwrap();
        assert!(cfg.contains(&format!("spira.lc_store_path={}", control_store(world).display())), "{cfg}");
        std::fs::write(control_store(world), "").unwrap();
        self.calls.lock().unwrap().push("control_store".into());
        Ok(())
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
fn only_this_worlds_serve_command_line_matches() {
    let w = Path::new("/tmp/w1");
    let line = |args: &[&str]| args.iter().flat_map(|a| a.bytes().chain([0])).collect::<Vec<u8>>();
    assert!(is_world_serve(&line(&["/tmp/w1/release/bin/spira-lc", "serve", "/tmp/w1/lc.sock"]), w));
    assert_eq!(serve_argv(w), vec!["/tmp/w1/release/bin/spira-lc", "serve", "/tmp/w1/lc.sock"]);
    for other in [
        line(&["/tmp/w2/release/bin/spira-lc", "serve", "/tmp/w2/lc.sock"]), // another world
        line(&["/opt/release/bin/spira-lc", "serve"]),                      // an installed release
        line(&["/tmp/w1/release/bin/spira-lc", "serve", "/tmp/w1/lc.sock", "x"]),
        line(&["sleep", "30"]),
        Vec::new(), // a zombie's cmdline
    ] {
        assert!(!is_world_serve(&other, w), "{other:?}");
    }
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
fn the_db_root_is_testenvs_tmpfs_root_when_nested_and_the_worlds_own_otherwise() {
    let w = Path::new("/w");
    assert_eq!(db_root(w, &clean), PathBuf::from("/w/db"));
    assert_eq!(db_root(w, &env_of(vec![("TESTDB_ROOT", String::new())])), PathBuf::from("/w/db"));
    assert_eq!(db_root(w, &env_of(vec![("TESTDB_ROOT", "/tmp/spira-testdb".into())])), PathBuf::from("/tmp/spira-testdb"));
}
