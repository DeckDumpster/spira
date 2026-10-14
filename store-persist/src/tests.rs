use super::ports::{Clock, Conn, Dolt};
use super::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use testkit::TempDir;

struct FakeClock(Cell<u64>);

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
    fn sleep(&self, secs: u64) {
        self.0.set(self.0.get() + secs * 1000);
    }
}

/// A sync copies the database's `payload` file into the copy beside a manifest; `bloat` adds
/// bytes the way chunks the live store has since collected stay in a real backup.
#[derive(Default)]
struct FakeDolt {
    live: RefCell<Vec<u32>>,
    syncs: RefCell<Vec<(Conn, String, PathBuf)>>,
    /// Fail this many upcoming syncs.
    fail_sync: Cell<usize>,
    fail_verify: Cell<bool>,
    bloat: Cell<usize>,
}

impl Dolt for FakeDolt {
    fn sync(&self, conn: &Conn, data: &Path, db: &str, dest: &Path) -> Result<(), String> {
        if self.fail_sync.get() > 0 {
            self.fail_sync.set(self.fail_sync.get() - 1);
            return Err("planted sync failure".into());
        }
        self.syncs.borrow_mut().push((conn.clone(), db.to_string(), dest.to_path_buf()));
        fs::create_dir_all(dest).unwrap();
        fs::copy(data.join(db).join(".dolt/noms/payload"), dest.join("payload")).unwrap();
        fs::write(dest.join("manifest"), db).unwrap();
        fs::write(dest.join("garbage"), vec![b'g'; self.bloat.get()]).unwrap();
        Ok(())
    }
    fn verify(&self, db_dir: &Path) -> Result<String, String> {
        if self.fail_verify.get() {
            return Err("planted corrupt copy".into());
        }
        fs::read_to_string(db_dir.join(".dolt/noms/payload")).map_err(|e| e.to_string())
    }
    fn alive(&self, pid: u32) -> bool {
        self.live.borrow().contains(&pid)
    }
}

struct World {
    _tmp: TempDir,
    cfg: Config,
    old: PathBuf,
    dolt: FakeDolt,
    clock: FakeClock,
}

fn put(p: &Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, s).unwrap();
}

fn make_db(data: &Path, db: &str, payload: &str) {
    put(&data.join(db).join(".dolt/noms/manifest"), "m");
    put(&data.join(db).join(".dolt/noms/payload"), payload);
    put(&data.join(db).join(".dolt/repo_state.json"), &format!("{{\"head\":\"refs/heads/main\",\"db\":\"{db}\"}}"));
}

fn yaml(data: &Path) -> String {
    format!("listener:\n  port: 3999\ndata_dir: \"{}\"\n", data.display())
}

/// An old on-disk store with two databases, a RAM data directory with install's yaml in it,
/// and an empty durable directory; values pinned off the shipped defaults.
fn world() -> World {
    let tmp = TempDir::new("store-persist");
    let (old, data, durable) = (tmp.join("old"), tmp.join("ram"), tmp.join("durable"));
    make_db(&old, "spira", "beads-v1");
    make_db(&old, "spira_lifecycle", "lc-v1");
    put(&old.join(".doltcfg/privileges.db"), "grants");
    put(&old.join("dolt-server.yaml"), &yaml(&old));
    put(&data.join("dolt-server.yaml"), &yaml(&data));
    let cfg = Config { data, durable: Some(durable), interval_secs: 7, max_lag_secs: 40, compact_pct: 150 };
    World { _tmp: tmp, cfg, old, dolt: FakeDolt::default(), clock: FakeClock(Cell::new(1_000)) }
}

fn seeded() -> World {
    let w = world();
    seed(&w.cfg, &w.old, &w.dolt, &w.clock).unwrap();
    w
}

fn layout(w: &World) -> Layout {
    w.cfg.layout().unwrap()
}

fn live_server(w: &World, pid: u32) {
    put(&w.cfg.data.join(".dolt/sql-server.info"), &format!("{pid}:3999:s3cret"));
    w.dolt.live.borrow_mut().push(pid);
}

fn values(drop: Option<&str>) -> BTreeMap<String, String> {
    let mut m: BTreeMap<String, String> = [
        (KEY_DATA, "/ram/dolt"),
        (KEY_DURABLE, "/disk/dolt-durable"),
        (KEY_INTERVAL, "7"),
        (KEY_MAX_LAG, "40"),
        (KEY_COMPACT_PCT, "150"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    if let Some(d) = drop {
        m.remove(d);
    }
    m
}

#[test]
fn config_reads_every_key_and_refuses_a_missing_one_by_name() {
    let c = Config::from_values(|k| values(None).get(k).cloned()).unwrap();
    assert_eq!(c.durable, Some(PathBuf::from("/disk/dolt-durable")));
    assert_eq!((c.interval_secs, c.max_lag_secs, c.compact_pct), (7, 40, 150));
    for k in KEYS {
        let e = Config::from_values(|q| values(Some(k)).get(q).cloned()).unwrap_err();
        assert!(e.contains(k), "{k}: {e}");
    }
}

#[test]
fn config_declared_empty_durable_means_no_copy_and_bad_values_are_refused() {
    let mut v = values(None);
    v.insert(KEY_DURABLE.into(), String::new());
    assert_eq!(Config::from_values(|k| v.get(k).cloned()).unwrap().durable, None);
    for (k, bad) in [(KEY_MAX_LAG, "7"), (KEY_COMPACT_PCT, "90"), (KEY_DURABLE, "/ram/dolt/copy"), (KEY_DURABLE, "rel"), (KEY_INTERVAL, "x")] {
        let mut v = values(None);
        v.insert(k.into(), bad.into());
        assert!(Config::from_values(|q| v.get(q).cloned()).is_err(), "{k}={bad} accepted");
    }
}

#[test]
fn seed_copies_every_database_and_its_meta_offline_without_the_old_server_yaml() {
    let w = seeded();
    let l = layout(&w);
    assert_eq!(durable_dbs(&l).unwrap().into_iter().collect::<Vec<_>>(), ["spira", "spira_lifecycle"]);
    assert!(w.dolt.syncs.borrow().iter().all(|(c, _, _)| *c == Conn::Offline));
    assert!(l.meta_db("spira").join("repo_state.json").is_file());
    assert_eq!(fs::read_to_string(l.meta_root().join("doltcfg/privileges.db")).unwrap(), "grants");
    assert!(!l.meta_root().join("dolt-server.yaml").exists(), "the old yaml names the old directory");
    assert!(l.marker().is_file());
    assert_eq!(read_state(&l).unwrap().dbs, ["spira", "spira_lifecycle"]);
    let again = seed(&w.cfg, &w.old, &w.dolt, &w.clock).unwrap_err();
    assert!(again.contains("already holds"), "{again}");
}

#[test]
fn seed_refuses_a_directory_a_live_server_holds() {
    let w = world();
    put(&w.old.join(".dolt/sql-server.info"), "77:3999:x");
    w.dolt.live.borrow_mut().push(77);
    let e = seed(&w.cfg, &w.old, &w.dolt, &w.clock).unwrap_err();
    assert!(e.contains("pid 77"), "{e}");
    assert!(!layout(&w).marker().exists());
}

#[test]
fn restore_fills_an_empty_ram_directory_from_the_copy() {
    let w = seeded();
    fs::remove_file(w.cfg.data.join("dolt-server.yaml")).unwrap();
    put(&layout(&w).meta_root().join("dolt-server.yaml"), &yaml(&w.cfg.data));
    let r = restore(&w.cfg, &w.dolt).unwrap();
    assert_eq!(r.restored, ["dolt-server.yaml", ".doltcfg/privileges.db", "spira", "spira_lifecycle"]);
    assert_eq!(fs::read_to_string(w.cfg.data.join("spira/.dolt/noms/payload")).unwrap(), "beads-v1");
    assert!(w.cfg.data.join("spira/.dolt/repo_state.json").is_file());
    let link = fs::read_link(w.cfg.data.join("spira/.dolt/git-remote-cache")).unwrap();
    assert_eq!(link, layout(&w).git_cache("spira"));
    assert!(!w.cfg.data.join(".dolt/store-persist").exists(), "staging is cleared");
    assert_eq!(data_dbs(&w.cfg.data).unwrap().len(), 2);
}

#[test]
fn restore_keeps_a_database_already_in_ram_because_it_is_newer() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    put(&w.cfg.data.join("spira/.dolt/noms/payload"), "beads-v2");
    let r = restore(&w.cfg, &w.dolt).unwrap();
    assert_eq!(r.kept, ["spira", "spira_lifecycle"]);
    assert!(r.restored.is_empty());
    assert_eq!(fs::read_to_string(w.cfg.data.join("spira/.dolt/noms/payload")).unwrap(), "beads-v2");
}

#[test]
fn restore_refuses_an_unseeded_copy_a_live_server_and_a_yaml_naming_another_directory() {
    let w = world();
    assert!(restore(&w.cfg, &w.dolt).unwrap_err().contains("never been seeded"));
    let w = seeded();
    live_server(&w, 4242);
    assert!(restore(&w.cfg, &w.dolt).unwrap_err().contains("pid 4242"));
    let w = seeded();
    put(&w.cfg.data.join("dolt-server.yaml"), &yaml(&w.old));
    let e = restore(&w.cfg, &w.dolt).unwrap_err();
    assert!(e.contains("would serve another directory"), "{e}");
    assert!(data_dbs(&w.cfg.data).unwrap().is_empty());
}

#[test]
fn a_copy_that_does_not_open_is_never_moved_into_place() {
    let w = seeded();
    w.dolt.fail_verify.set(true);
    let e = restore(&w.cfg, &w.dolt).unwrap_err();
    assert!(e.contains("does not open"), "{e}");
    assert!(data_dbs(&w.cfg.data).unwrap().is_empty());
}

#[test]
fn without_a_durable_copy_a_ram_backed_directory_is_refused() {
    let mut w = world();
    w.cfg.durable = None;
    let tmpfs = is_tmpfs(&w.cfg.data).unwrap();
    match restore(&w.cfg, &w.dolt) {
        Err(e) => assert!(tmpfs && e.contains("RAM-backed"), "{e}"),
        Ok(r) => assert!(!tmpfs && r.notes[0].contains("served as it is"), "{r:?}"),
    }
}

#[test]
fn is_tmpfs_tells_a_ram_filesystem_from_a_disk_one() {
    let proc_is_tmpfs = is_tmpfs(Path::new("/proc")).unwrap();
    assert!(!proc_is_tmpfs, "procfs is not tmpfs — a matcher that says yes to everything is caught here");
    if Path::new("/dev/shm").is_dir() {
        assert!(is_tmpfs(Path::new("/dev/shm/not-yet-created")).unwrap(), "/dev/shm is tmpfs on every Linux this runs on");
    }
}

#[test]
fn an_online_pass_goes_through_the_server_and_records_its_start() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    live_server(&w, 4242);
    w.clock.0.set(5_000);
    put(&w.cfg.data.join("spira/.dolt/noms/payload"), "beads-v3");
    let st = pass(&w.cfg, Mode::Online, &w.dolt, &w.clock).unwrap();
    assert_eq!(st.start_ms, 5_000);
    assert_eq!(read_state(&layout(&w)).unwrap(), st);
    let last = w.dolt.syncs.borrow().last().cloned().unwrap();
    assert_eq!(last.0, Conn::Server { port: 3999, secret: "s3cret".into() });
    assert_eq!(fs::read_to_string(layout(&w).db("spira").join("payload")).unwrap(), "beads-v3");
    assert!(layout(&w).meta_root().join("dolt-server.yaml").is_file(), "the live yaml is kept for the next reboot");
}

#[test]
fn a_pass_needs_the_mode_its_server_state_allows() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    assert!(pass(&w.cfg, Mode::Online, &w.dolt, &w.clock).unwrap_err().contains("no live dolt server"));
    put(&w.cfg.data.join(".dolt/sql-server.info"), "999:3999:x");
    pass(&w.cfg, Mode::Offline, &w.dolt, &w.clock).expect("a dead server's info file does not block the stop-time pass");
    live_server(&w, 4242);
    assert!(pass(&w.cfg, Mode::Offline, &w.dolt, &w.clock).unwrap_err().contains("pid 4242"));
}

#[test]
fn a_pass_before_seeding_is_refused() {
    let w = world();
    assert!(pass(&w.cfg, Mode::Offline, &w.dolt, &w.clock).unwrap_err().contains("never been seeded"));
}

#[test]
fn a_pass_never_leaves_a_durable_database_behind() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    fs::remove_dir_all(w.cfg.data.join("spira_lifecycle")).unwrap();
    let before = read_state(&layout(&w)).unwrap();
    let e = pass(&w.cfg, Mode::Offline, &w.dolt, &w.clock).unwrap_err();
    assert!(e.contains("spira_lifecycle"), "{e}");
    assert_eq!(read_state(&layout(&w)).unwrap(), before, "a refused pass records nothing");
}

#[test]
fn a_failed_pass_keeps_the_last_good_start_so_age_turns_red() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    live_server(&w, 4242);
    w.clock.0.set(2_000);
    pass(&w.cfg, Mode::Online, &w.dolt, &w.clock).unwrap();
    w.dolt.fail_sync.set(usize::MAX);
    let mut log = Vec::new();
    run(&w.cfg, &w.dolt, &w.clock, Some(8), &mut |l| log.push(l.to_string()));
    assert_eq!(read_state(&layout(&w)).unwrap().start_ms, 2_000);
    assert_eq!(age(&w.cfg, 2_000 + 40_000).unwrap(), (40_000, false));
    assert_eq!(age(&w.cfg, 2_000 + 40_001).unwrap(), (40_001, true));
    assert_eq!(log.len(), 8);
    w.dolt.fail_sync.set(0);
    log.clear();
    run(&w.cfg, &w.dolt, &w.clock, Some(3), &mut |l| log.push(l.to_string()));
    assert_eq!(log.len(), 0, "a healthy daemon is silent: {log:?}");
}

#[test]
fn the_daemon_reports_a_recovery_once() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    live_server(&w, 4242);
    w.dolt.fail_sync.set(2);
    let mut log = Vec::new();
    run(&w.cfg, &w.dolt, &w.clock, Some(5), &mut |l| log.push(l.to_string()));
    assert_eq!(log.len(), 3, "{log:?}");
    assert!(log[0].contains("failed") && log[1].contains("failed") && log[2].contains("recovered"), "{log:?}");
    assert_eq!(w.clock.now_ms(), 1_000 + 4 * 7 * 1000, "one interval between passes");
}

#[test]
fn state_round_trips() {
    let st = State { start_ms: 10, finish_ms: 12, dbs: vec!["a".into(), "b".into()] };
    assert_eq!(State::parse(&st.render()).unwrap(), st);
    assert!(State::parse("finish_ms=1\ndbs=\n").unwrap_err().contains("start_ms"));
}

#[test]
fn put_durable_leaves_identical_bytes_alone() {
    let t = TempDir::new("store-persist-put");
    let p = t.join("a/b");
    assert!(put_durable(&p, b"x").unwrap());
    assert!(!put_durable(&p, b"x").unwrap());
    assert!(put_durable(&p, b"y").unwrap());
    assert_eq!(fs::read(&p).unwrap(), b"y");
}

#[test]
fn compaction_rebuilds_only_a_copy_past_its_ratio_and_swaps_it_whole() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    live_server(&w, 4242);
    let l = layout(&w);
    let out = compact(&w.cfg, &w.dolt).unwrap();
    assert!(out.iter().all(|o| o.contains("within")), "{out:?}");
    w.dolt.bloat.set(4096);
    pass(&w.cfg, Mode::Online, &w.dolt, &w.clock).unwrap();
    let bloated = tree_bytes(&l.db("spira"));
    w.dolt.bloat.set(0);
    let out = compact(&w.cfg, &w.dolt).unwrap();
    assert!(out.iter().all(|o| o.contains("rebuilt")), "{out:?}");
    assert!(tree_bytes(&l.db("spira")) < bloated);
    assert_eq!(fs::read_to_string(l.db("spira").join("payload")).unwrap(), "beads-v1");
    assert!(!l.compact_dir().join("spira").exists(), "the superseded copy is removed");
}
